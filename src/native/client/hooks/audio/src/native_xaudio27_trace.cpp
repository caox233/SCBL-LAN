#include "native_xaudio27_trace.hpp"
#include "native_xaudio27_relay.hpp"
#include "native_master_output.hpp"

#include <mmdeviceapi.h>

#include <algorithm>
#include <array>
#include <atomic>
#include <cstdio>
#include <cstring>
#include <intrin.h>
#include <cstdint>
#include <string>

namespace scbl::audio {
namespace {

constexpr UINT32 kLogWarning = 1;
constexpr UINT32 kLogInfo = 2;
constexpr UINT32 kEngineSlots = 16;
constexpr UINT32 kVoiceSlots = 29;
constexpr UINT32 kMaxTracedObjects = 2048;

enum class ObjectKind : UINT32 {
    Engine = 0,
    Source = 1,
    Submix = 2,
    Master = 3,
};

struct TraceEntry {
    void* object = nullptr;
    void** original = nullptr;
    std::array<void*, kVoiceSlots> traced{};
    ObjectKind kind = ObjectKind::Engine;
    UINT32 slots = 0;
    bool active = false;
};

using CoCreateInstanceFn = HRESULT(WINAPI*)(REFCLSID, LPUNKNOWN, DWORD, REFIID, LPVOID*);

SRWLOCK g_entries_lock = SRWLOCK_INIT;
std::array<TraceEntry, kMaxTracedObjects> g_entries{};
std::array<std::array<std::atomic<UINT64>, kVoiceSlots>, 4> g_counts{};
std::array<std::atomic<UINT32>, 4> g_seen_masks{};
std::atomic<UINT32> g_created_sources_total{0};
std::atomic<UINT32> g_created_submixes_total{0};
std::atomic<UINT32> g_active_masters{0};
CoCreateInstanceFn g_original_co_create = nullptr;
void** g_iat_slot = nullptr;
NativeTraceLogFn g_log = nullptr;
std::atomic<IXAudio2EngineCallback*> g_engine_callback{nullptr};

std::atomic<void*> g_game_audio_manager{nullptr};
std::atomic<uintptr_t> g_game_audio_manager_argument1{0};
std::atomic<uintptr_t> g_game_audio_manager_argument2{0};
std::atomic<uintptr_t> g_game_audio_manager_argument3{0};
std::atomic<DWORD> g_game_audio_manager_thread{0};
std::atomic<bool> g_audio_reset_pending{false};
std::atomic<bool> g_audio_reset_running{false};
std::atomic<bool> g_audio_recovery_waiting_for_game_initialize{false};
std::atomic<ULONGLONG> g_audio_recovery_shutdown_tick{0};
std::atomic<bool> g_audio_recovery_stop{false};
std::atomic<bool> g_audio_safe_point_seen{false};
HANDLE g_audio_recovery_thread = nullptr;

void write_log(UINT32 level, const char* message) noexcept;

#if defined(_M_IX86)
using GameAudioManagerInitializeFn = HRESULT(__thiscall*)(void*, uintptr_t, uintptr_t, uintptr_t);

constexpr uintptr_t kGameAudioManagerInitializeRva = 0x01B57A80;
constexpr SIZE_T kGameAudioManagerInitializePatchSize = 6;
constexpr std::array<BYTE, kGameAudioManagerInitializePatchSize> kGameAudioManagerInitializePrefix = {
    0x51,       // push ecx
    0x56,       // push esi
    0x6A, 0x00, // push 0
    0x6A, 0x00, // push 0
};
constexpr UINT32 kGameAudioManagerVtableSlots = 86;

GameAudioManagerInitializeFn g_original_game_audio_manager_initialize = nullptr;
void* g_game_audio_manager_initialize_target = nullptr;
void* g_game_audio_manager_initialize_trampoline = nullptr;
std::array<void*, kGameAudioManagerVtableSlots> g_game_audio_manager_original_vtable{};
std::array<void*, kGameAudioManagerVtableSlots> g_game_audio_manager_traced_vtable{};
void** g_game_audio_manager_original_vtable_pointer = nullptr;
std::array<std::atomic<UINT64>, kGameAudioManagerVtableSlots> g_game_audio_manager_slot_counts{};
std::atomic<bool> g_game_audio_manager_vtable_installed{false};

bool install_game_audio_manager_vtable_safe_points(void* manager) noexcept;

const char* memory_kind(uintptr_t value) noexcept {
    if (value == 0) return "null";
    MEMORY_BASIC_INFORMATION information{};
    if (VirtualQuery(reinterpret_cast<const void*>(value), &information, sizeof(information)) == 0
        || information.State != MEM_COMMIT) {
        return "unmapped";
    }
    if (information.Type == MEM_IMAGE) return "image";
    if (information.Type == MEM_MAPPED) return "mapped";
    if (information.Type == MEM_PRIVATE) return "private";
    return "other";
}

HRESULT __fastcall traced_game_audio_manager_initialize(
    void* self,
    void*,
    uintptr_t argument1,
    uintptr_t argument2,
    uintptr_t argument3) noexcept {
    char message[320]{};
    std::snprintf(message, sizeof(message),
        "SCBL_AUDIO_NATIVE_TRACE manager-init phase=enter self=0x%08X thread=%lu arg1=0x%08X/%s arg2=0x%08X/%s arg3=0x%08X/%s",
        static_cast<unsigned int>(reinterpret_cast<uintptr_t>(self)),
        static_cast<unsigned long>(GetCurrentThreadId()),
        static_cast<unsigned int>(argument1), memory_kind(argument1),
        static_cast<unsigned int>(argument2), memory_kind(argument2),
        static_cast<unsigned int>(argument3), memory_kind(argument3));
    write_log(kLogInfo, message);

    const HRESULT result = g_original_game_audio_manager_initialize != nullptr
        ? g_original_game_audio_manager_initialize(self, argument1, argument2, argument3)
        : E_UNEXPECTED;

    if (SUCCEEDED(result)) {
        g_game_audio_manager_argument1.store(argument1, std::memory_order_release);
        g_game_audio_manager_argument2.store(argument2, std::memory_order_release);
        g_game_audio_manager_argument3.store(argument3, std::memory_order_release);
        g_game_audio_manager_thread.store(GetCurrentThreadId(), std::memory_order_release);
        g_game_audio_manager.store(self, std::memory_order_release);
        install_game_audio_manager_vtable_safe_points(self);
        if (g_audio_recovery_waiting_for_game_initialize.exchange(false, std::memory_order_acq_rel)) {
            char recovery_message[192]{};
            const ULONGLONG elapsed = GetTickCount64()
                - g_audio_recovery_shutdown_tick.load(std::memory_order_acquire);
            std::snprintf(recovery_message, sizeof(recovery_message),
                "SCBL_AUDIO_RECOVERY reset=end hr=0x%08lX owner=game-lifecycle elapsed-ms=%llu",
                static_cast<unsigned long>(result), static_cast<unsigned long long>(elapsed));
            write_log(kLogInfo, recovery_message);
        }
    }

    const auto base = reinterpret_cast<uintptr_t>(GetModuleHandleW(nullptr));
    void* engine_owner = nullptr;
    if (base != 0) {
        engine_owner = *reinterpret_cast<void**>(base + 0x030BAB48);
    }
    std::snprintf(message, sizeof(message),
        "SCBL_AUDIO_NATIVE_TRACE manager-init phase=leave self=0x%08X thread=%lu hr=0x%08lX engine-owner=0x%08X",
        static_cast<unsigned int>(reinterpret_cast<uintptr_t>(self)),
        static_cast<unsigned long>(GetCurrentThreadId()),
        static_cast<unsigned long>(result),
        static_cast<unsigned int>(reinterpret_cast<uintptr_t>(engine_owner)));
    write_log(SUCCEEDED(result) ? kLogInfo : kLogWarning, message);
    return result;
}

template<typename Fn>
bool install_game_detour(
    uintptr_t relative_address,
    const BYTE* expected,
    SIZE_T patch_size,
    void* hook,
    Fn* original,
    void** target_out,
    void** trampoline_out,
    const char* signature_failure,
    const char* allocation_failure,
    const char* protection_failure) noexcept {
    if (*target_out != nullptr) return true;
    auto* base = reinterpret_cast<BYTE*>(GetModuleHandleW(nullptr));
    if (base == nullptr) return false;
    const auto* dos = reinterpret_cast<const IMAGE_DOS_HEADER*>(base);
    if (dos->e_magic != IMAGE_DOS_SIGNATURE) return false;
    const auto* nt = reinterpret_cast<const IMAGE_NT_HEADERS*>(base + dos->e_lfanew);
    if (nt->Signature != IMAGE_NT_SIGNATURE || relative_address + patch_size > nt->OptionalHeader.SizeOfImage) {
        write_log(kLogInfo, signature_failure);
        return false;
    }
    auto* target = base + relative_address;
    if (std::memcmp(target, expected, patch_size) != 0) {
        write_log(kLogWarning, signature_failure);
        return false;
    }
    auto* trampoline = static_cast<BYTE*>(VirtualAlloc(
        nullptr, patch_size + 5, MEM_COMMIT | MEM_RESERVE, PAGE_EXECUTE_READWRITE));
    if (trampoline == nullptr) {
        write_log(kLogWarning, allocation_failure);
        return false;
    }
    std::memcpy(trampoline, target, patch_size);
    trampoline[patch_size] = 0xE9;
    *reinterpret_cast<INT32*>(trampoline + patch_size + 1) = static_cast<INT32>(
        reinterpret_cast<uintptr_t>(target + patch_size)
        - reinterpret_cast<uintptr_t>(trampoline + patch_size + 5));
    DWORD old_protection = 0;
    if (!VirtualProtect(target, patch_size, PAGE_EXECUTE_READWRITE, &old_protection)) {
        VirtualFree(trampoline, 0, MEM_RELEASE);
        write_log(kLogWarning, protection_failure);
        return false;
    }
    target[0] = 0xE9;
    *reinterpret_cast<INT32*>(target + 1) = static_cast<INT32>(
        reinterpret_cast<uintptr_t>(hook) - reinterpret_cast<uintptr_t>(target + 5));
    for (SIZE_T index = 5; index < patch_size; ++index) target[index] = 0x90;
    DWORD ignored = 0;
    VirtualProtect(target, patch_size, old_protection, &ignored);
    FlushInstructionCache(GetCurrentProcess(), target, patch_size);
    *original = reinterpret_cast<Fn>(trampoline);
    *target_out = target;
    *trampoline_out = trampoline;
    return true;
}

bool install_game_audio_manager_initialize_trace() noexcept {
    if (g_game_audio_manager_initialize_target != nullptr) return true;
    if (!install_game_detour(
            kGameAudioManagerInitializeRva,
            kGameAudioManagerInitializePrefix.data(),
            kGameAudioManagerInitializePatchSize,
            reinterpret_cast<void*>(traced_game_audio_manager_initialize),
            &g_original_game_audio_manager_initialize,
            &g_game_audio_manager_initialize_target,
            &g_game_audio_manager_initialize_trampoline,
            "SCBL_AUDIO_NATIVE_TRACE manager-init-hook=false reason=signature-mismatch-or-host",
            "SCBL_AUDIO_NATIVE_TRACE manager-init-hook=false reason=trampoline-allocation",
            "SCBL_AUDIO_NATIVE_TRACE manager-init-hook=false reason=target-protection")) {
        return false;
    }
    write_log(kLogInfo, "SCBL_AUDIO_NATIVE_TRACE manager-init-hook=true behavior=passthrough");
    return true;
}

bool reset_game_audio_manager_on_owner_thread() noexcept {
    void* manager = g_game_audio_manager.load(std::memory_order_acquire);
    const DWORD owner_thread = g_game_audio_manager_thread.load(std::memory_order_acquire);
    if (manager == nullptr || owner_thread == 0 || GetCurrentThreadId() != owner_thread
        || g_original_game_audio_manager_initialize == nullptr) {
        return false;
    }
    if (g_audio_reset_running.exchange(true, std::memory_order_acq_rel)) return false;

    bool attempted = false;
    auto** vtable = *reinterpret_cast<void***>(manager);
    const auto base = reinterpret_cast<uintptr_t>(GetModuleHandleW(nullptr));
    const auto expected_initialize = reinterpret_cast<void*>(base + kGameAudioManagerInitializeRva);
    const auto expected_shutdown = reinterpret_cast<void*>(base + 0x01B57B50);
    const bool traced_vtable = vtable == g_game_audio_manager_traced_vtable.data();
    void* initialize_entry = traced_vtable
        ? g_game_audio_manager_original_vtable[0x50 / sizeof(void*)]
        : (vtable != nullptr ? vtable[0x50 / sizeof(void*)] : nullptr);
    void* shutdown_entry = traced_vtable
        ? g_game_audio_manager_original_vtable[0x54 / sizeof(void*)]
        : (vtable != nullptr ? vtable[0x54 / sizeof(void*)] : nullptr);
    if (initialize_entry != expected_initialize || shutdown_entry != expected_shutdown) {
        write_log(kLogWarning, "SCBL_AUDIO_RECOVERY reset=false reason=manager-vtable-mismatch");
    } else {
        using ShutdownFn = void(__thiscall*)(void*);
        const auto shutdown = reinterpret_cast<ShutdownFn>(shutdown_entry);
        write_log(kLogInfo,
            "SCBL_AUDIO_RECOVERY reset=begin thread=owner owner=game-lifecycle");
        g_audio_recovery_shutdown_tick.store(GetTickCount64(), std::memory_order_release);
        g_audio_recovery_waiting_for_game_initialize.store(true, std::memory_order_release);
        shutdown(manager);
        write_log(kLogInfo,
            "SCBL_AUDIO_RECOVERY shutdown=end waiting=game-lifecycle-initialize");
        attempted = true;
    }
    g_audio_reset_pending.store(false, std::memory_order_release);
    g_audio_reset_running.store(false, std::memory_order_release);
    return attempted;
}

void maybe_reset_game_audio_on_owner_thread(void* safe_point_manager) noexcept {
    if (!g_audio_reset_pending.load(std::memory_order_acquire)) return;
    if (safe_point_manager == nullptr
        || safe_point_manager != g_game_audio_manager.load(std::memory_order_acquire)) return;
    const DWORD owner_thread = g_game_audio_manager_thread.load(std::memory_order_acquire);
    if (owner_thread == 0 || GetCurrentThreadId() != owner_thread) return;
    if (!g_audio_safe_point_seen.exchange(true, std::memory_order_acq_rel)) {
        char message[128]{};
        std::snprintf(message, sizeof(message),
            "SCBL_AUDIO_RECOVERY safe-point=manager-call-boundary thread=%lu",
            static_cast<unsigned long>(owner_thread));
        write_log(kLogInfo, message);
    }
    reset_game_audio_manager_on_owner_thread();
}

void trace_game_audio_manager_call(void* self, UINT32 slot) noexcept {
    if (slot >= kGameAudioManagerVtableSlots) return;
    if (g_game_audio_manager_slot_counts[slot].fetch_add(1, std::memory_order_relaxed) == 0) {
        char message[176]{};
        std::snprintf(message, sizeof(message),
            "SCBL_AUDIO_RECOVERY manager-call-first slot=%u self=0x%08X thread=%lu owner-thread=%lu",
            slot,
            static_cast<unsigned int>(reinterpret_cast<uintptr_t>(self)),
            static_cast<unsigned long>(GetCurrentThreadId()),
            static_cast<unsigned long>(g_game_audio_manager_thread.load(std::memory_order_acquire)));
        write_log(kLogInfo, message);
    }
    maybe_reset_game_audio_on_owner_thread(self);
}

#define SCBL_DEFINE_MANAGER_SAFE_POINT(slot) \
    __declspec(naked) void manager_safe_point_##slot() { \
        __asm pushfd \
        __asm pushad \
        __asm mov eax, dword ptr [esp + 24] \
        __asm push slot \
        __asm push eax \
        __asm call trace_game_audio_manager_call \
        __asm add esp, 8 \
        __asm popad \
        __asm popfd \
        __asm jmp dword ptr [g_game_audio_manager_original_vtable + slot * 4] \
    }

SCBL_DEFINE_MANAGER_SAFE_POINT(0)
SCBL_DEFINE_MANAGER_SAFE_POINT(1)
SCBL_DEFINE_MANAGER_SAFE_POINT(2)
SCBL_DEFINE_MANAGER_SAFE_POINT(3)
SCBL_DEFINE_MANAGER_SAFE_POINT(4)
SCBL_DEFINE_MANAGER_SAFE_POINT(5)
SCBL_DEFINE_MANAGER_SAFE_POINT(6)
SCBL_DEFINE_MANAGER_SAFE_POINT(7)
SCBL_DEFINE_MANAGER_SAFE_POINT(8)
SCBL_DEFINE_MANAGER_SAFE_POINT(9)
SCBL_DEFINE_MANAGER_SAFE_POINT(10)
SCBL_DEFINE_MANAGER_SAFE_POINT(11)
SCBL_DEFINE_MANAGER_SAFE_POINT(12)
SCBL_DEFINE_MANAGER_SAFE_POINT(13)
SCBL_DEFINE_MANAGER_SAFE_POINT(14)
SCBL_DEFINE_MANAGER_SAFE_POINT(15)
SCBL_DEFINE_MANAGER_SAFE_POINT(16)
SCBL_DEFINE_MANAGER_SAFE_POINT(17)
SCBL_DEFINE_MANAGER_SAFE_POINT(18)
SCBL_DEFINE_MANAGER_SAFE_POINT(19)
SCBL_DEFINE_MANAGER_SAFE_POINT(20)
SCBL_DEFINE_MANAGER_SAFE_POINT(21)
SCBL_DEFINE_MANAGER_SAFE_POINT(22)
SCBL_DEFINE_MANAGER_SAFE_POINT(23)
SCBL_DEFINE_MANAGER_SAFE_POINT(24)
SCBL_DEFINE_MANAGER_SAFE_POINT(25)
SCBL_DEFINE_MANAGER_SAFE_POINT(26)
SCBL_DEFINE_MANAGER_SAFE_POINT(27)
SCBL_DEFINE_MANAGER_SAFE_POINT(28)
SCBL_DEFINE_MANAGER_SAFE_POINT(29)
SCBL_DEFINE_MANAGER_SAFE_POINT(30)
SCBL_DEFINE_MANAGER_SAFE_POINT(31)
SCBL_DEFINE_MANAGER_SAFE_POINT(32)
SCBL_DEFINE_MANAGER_SAFE_POINT(33)
SCBL_DEFINE_MANAGER_SAFE_POINT(34)
SCBL_DEFINE_MANAGER_SAFE_POINT(35)
SCBL_DEFINE_MANAGER_SAFE_POINT(36)
SCBL_DEFINE_MANAGER_SAFE_POINT(37)
SCBL_DEFINE_MANAGER_SAFE_POINT(38)
SCBL_DEFINE_MANAGER_SAFE_POINT(39)
SCBL_DEFINE_MANAGER_SAFE_POINT(40)
SCBL_DEFINE_MANAGER_SAFE_POINT(41)
SCBL_DEFINE_MANAGER_SAFE_POINT(42)
SCBL_DEFINE_MANAGER_SAFE_POINT(43)
SCBL_DEFINE_MANAGER_SAFE_POINT(44)
SCBL_DEFINE_MANAGER_SAFE_POINT(45)
SCBL_DEFINE_MANAGER_SAFE_POINT(46)
SCBL_DEFINE_MANAGER_SAFE_POINT(47)
SCBL_DEFINE_MANAGER_SAFE_POINT(48)
SCBL_DEFINE_MANAGER_SAFE_POINT(49)
SCBL_DEFINE_MANAGER_SAFE_POINT(50)
SCBL_DEFINE_MANAGER_SAFE_POINT(51)
SCBL_DEFINE_MANAGER_SAFE_POINT(52)
SCBL_DEFINE_MANAGER_SAFE_POINT(53)
SCBL_DEFINE_MANAGER_SAFE_POINT(54)
SCBL_DEFINE_MANAGER_SAFE_POINT(55)
SCBL_DEFINE_MANAGER_SAFE_POINT(56)
SCBL_DEFINE_MANAGER_SAFE_POINT(57)
SCBL_DEFINE_MANAGER_SAFE_POINT(58)
SCBL_DEFINE_MANAGER_SAFE_POINT(59)
SCBL_DEFINE_MANAGER_SAFE_POINT(60)
SCBL_DEFINE_MANAGER_SAFE_POINT(61)
SCBL_DEFINE_MANAGER_SAFE_POINT(62)
SCBL_DEFINE_MANAGER_SAFE_POINT(63)
SCBL_DEFINE_MANAGER_SAFE_POINT(64)
SCBL_DEFINE_MANAGER_SAFE_POINT(65)
SCBL_DEFINE_MANAGER_SAFE_POINT(66)
SCBL_DEFINE_MANAGER_SAFE_POINT(67)
SCBL_DEFINE_MANAGER_SAFE_POINT(68)
SCBL_DEFINE_MANAGER_SAFE_POINT(69)
SCBL_DEFINE_MANAGER_SAFE_POINT(70)
SCBL_DEFINE_MANAGER_SAFE_POINT(71)
SCBL_DEFINE_MANAGER_SAFE_POINT(72)
SCBL_DEFINE_MANAGER_SAFE_POINT(73)
SCBL_DEFINE_MANAGER_SAFE_POINT(74)
SCBL_DEFINE_MANAGER_SAFE_POINT(75)
SCBL_DEFINE_MANAGER_SAFE_POINT(76)
SCBL_DEFINE_MANAGER_SAFE_POINT(77)
SCBL_DEFINE_MANAGER_SAFE_POINT(78)
SCBL_DEFINE_MANAGER_SAFE_POINT(79)
SCBL_DEFINE_MANAGER_SAFE_POINT(80)
SCBL_DEFINE_MANAGER_SAFE_POINT(81)
SCBL_DEFINE_MANAGER_SAFE_POINT(82)
SCBL_DEFINE_MANAGER_SAFE_POINT(83)
SCBL_DEFINE_MANAGER_SAFE_POINT(84)
SCBL_DEFINE_MANAGER_SAFE_POINT(85)

#undef SCBL_DEFINE_MANAGER_SAFE_POINT

const std::array<void*, kGameAudioManagerVtableSlots> kGameAudioManagerSafePoints = {
    reinterpret_cast<void*>(manager_safe_point_0), reinterpret_cast<void*>(manager_safe_point_1),
    reinterpret_cast<void*>(manager_safe_point_2), reinterpret_cast<void*>(manager_safe_point_3),
    reinterpret_cast<void*>(manager_safe_point_4), reinterpret_cast<void*>(manager_safe_point_5),
    reinterpret_cast<void*>(manager_safe_point_6), reinterpret_cast<void*>(manager_safe_point_7),
    reinterpret_cast<void*>(manager_safe_point_8), reinterpret_cast<void*>(manager_safe_point_9),
    reinterpret_cast<void*>(manager_safe_point_10), reinterpret_cast<void*>(manager_safe_point_11),
    reinterpret_cast<void*>(manager_safe_point_12), reinterpret_cast<void*>(manager_safe_point_13),
    reinterpret_cast<void*>(manager_safe_point_14), reinterpret_cast<void*>(manager_safe_point_15),
    reinterpret_cast<void*>(manager_safe_point_16), reinterpret_cast<void*>(manager_safe_point_17),
    reinterpret_cast<void*>(manager_safe_point_18), reinterpret_cast<void*>(manager_safe_point_19),
    reinterpret_cast<void*>(manager_safe_point_20), reinterpret_cast<void*>(manager_safe_point_21),
    reinterpret_cast<void*>(manager_safe_point_22), reinterpret_cast<void*>(manager_safe_point_23),
    reinterpret_cast<void*>(manager_safe_point_24), reinterpret_cast<void*>(manager_safe_point_25),
    reinterpret_cast<void*>(manager_safe_point_26), reinterpret_cast<void*>(manager_safe_point_27),
    reinterpret_cast<void*>(manager_safe_point_28), reinterpret_cast<void*>(manager_safe_point_29),
    reinterpret_cast<void*>(manager_safe_point_30), reinterpret_cast<void*>(manager_safe_point_31),
    reinterpret_cast<void*>(manager_safe_point_32), reinterpret_cast<void*>(manager_safe_point_33),
    reinterpret_cast<void*>(manager_safe_point_34), reinterpret_cast<void*>(manager_safe_point_35),
    reinterpret_cast<void*>(manager_safe_point_36), reinterpret_cast<void*>(manager_safe_point_37),
    reinterpret_cast<void*>(manager_safe_point_38), reinterpret_cast<void*>(manager_safe_point_39),
    reinterpret_cast<void*>(manager_safe_point_40), reinterpret_cast<void*>(manager_safe_point_41),
    reinterpret_cast<void*>(manager_safe_point_42), reinterpret_cast<void*>(manager_safe_point_43),
    reinterpret_cast<void*>(manager_safe_point_44), reinterpret_cast<void*>(manager_safe_point_45),
    reinterpret_cast<void*>(manager_safe_point_46), reinterpret_cast<void*>(manager_safe_point_47),
    reinterpret_cast<void*>(manager_safe_point_48), reinterpret_cast<void*>(manager_safe_point_49),
    reinterpret_cast<void*>(manager_safe_point_50), reinterpret_cast<void*>(manager_safe_point_51),
    reinterpret_cast<void*>(manager_safe_point_52), reinterpret_cast<void*>(manager_safe_point_53),
    reinterpret_cast<void*>(manager_safe_point_54), reinterpret_cast<void*>(manager_safe_point_55),
    reinterpret_cast<void*>(manager_safe_point_56), reinterpret_cast<void*>(manager_safe_point_57),
    reinterpret_cast<void*>(manager_safe_point_58), reinterpret_cast<void*>(manager_safe_point_59),
    reinterpret_cast<void*>(manager_safe_point_60), reinterpret_cast<void*>(manager_safe_point_61),
    reinterpret_cast<void*>(manager_safe_point_62), reinterpret_cast<void*>(manager_safe_point_63),
    reinterpret_cast<void*>(manager_safe_point_64), reinterpret_cast<void*>(manager_safe_point_65),
    reinterpret_cast<void*>(manager_safe_point_66), reinterpret_cast<void*>(manager_safe_point_67),
    reinterpret_cast<void*>(manager_safe_point_68), reinterpret_cast<void*>(manager_safe_point_69),
    reinterpret_cast<void*>(manager_safe_point_70), reinterpret_cast<void*>(manager_safe_point_71),
    reinterpret_cast<void*>(manager_safe_point_72), reinterpret_cast<void*>(manager_safe_point_73),
    reinterpret_cast<void*>(manager_safe_point_74), reinterpret_cast<void*>(manager_safe_point_75),
    reinterpret_cast<void*>(manager_safe_point_76), reinterpret_cast<void*>(manager_safe_point_77),
    reinterpret_cast<void*>(manager_safe_point_78), reinterpret_cast<void*>(manager_safe_point_79),
    reinterpret_cast<void*>(manager_safe_point_80), reinterpret_cast<void*>(manager_safe_point_81),
    reinterpret_cast<void*>(manager_safe_point_82), reinterpret_cast<void*>(manager_safe_point_83),
    reinterpret_cast<void*>(manager_safe_point_84), reinterpret_cast<void*>(manager_safe_point_85),
};

bool install_game_audio_manager_vtable_safe_points(void* manager) noexcept {
    if (manager == nullptr) return false;
    if (g_game_audio_manager_vtable_installed.load(std::memory_order_acquire)) {
        auto** current = *reinterpret_cast<void***>(manager);
        if (current == g_game_audio_manager_traced_vtable.data()) return true;
        if (current != g_game_audio_manager_original_vtable_pointer) {
            write_log(kLogWarning, "SCBL_AUDIO_RECOVERY manager-vtable-hook=false reason=unexpected-replacement");
            return false;
        }
        InterlockedExchangePointer(
            reinterpret_cast<void* volatile*>(manager),
            g_game_audio_manager_traced_vtable.data());
        return true;
    }
    auto** original = *reinterpret_cast<void***>(manager);
    const auto base = reinterpret_cast<uintptr_t>(GetModuleHandleW(nullptr));
    if (original == nullptr
        || original[0x50 / sizeof(void*)] != reinterpret_cast<void*>(base + kGameAudioManagerInitializeRva)
        || original[0x54 / sizeof(void*)] != reinterpret_cast<void*>(base + 0x01B57B50)) {
        write_log(kLogWarning, "SCBL_AUDIO_RECOVERY manager-vtable-hook=false reason=signature-mismatch");
        return false;
    }
    std::copy_n(original, kGameAudioManagerVtableSlots, g_game_audio_manager_original_vtable.begin());
    std::copy(kGameAudioManagerSafePoints.begin(), kGameAudioManagerSafePoints.end(),
        g_game_audio_manager_traced_vtable.begin());
    g_game_audio_manager_original_vtable_pointer = original;
    InterlockedExchangePointer(
        reinterpret_cast<void* volatile*>(manager),
        g_game_audio_manager_traced_vtable.data());
    g_game_audio_manager_vtable_installed.store(true, std::memory_order_release);
    write_log(kLogInfo, "SCBL_AUDIO_RECOVERY manager-vtable-hook=true behavior=call-boundary-safe-points");
    return true;
}

std::wstring default_render_endpoint_key(IMMDeviceEnumerator* enumerator) {
    if (enumerator == nullptr) return {};
    std::wstring result;
    for (const ERole role : {eConsole, eMultimedia}) {
        IMMDevice* device = nullptr;
        LPWSTR id = nullptr;
        if (SUCCEEDED(enumerator->GetDefaultAudioEndpoint(eRender, role, &device)) && device != nullptr) {
            if (SUCCEEDED(device->GetId(&id)) && id != nullptr) {
                result.append(id);
            }
            if (id != nullptr) CoTaskMemFree(id);
            device->Release();
        }
        result.push_back(L'\n');
    }
    return result;
}

std::wstring default_render_endpoint_id(IMMDeviceEnumerator* enumerator) {
    if (enumerator == nullptr) return {};
    IMMDevice* device = nullptr;
    LPWSTR id = nullptr;
    std::wstring result;
    if (SUCCEEDED(enumerator->GetDefaultAudioEndpoint(eRender, eMultimedia, &device))
        && device != nullptr) {
        if (SUCCEEDED(device->GetId(&id)) && id != nullptr) result = id;
        if (id != nullptr) CoTaskMemFree(id);
        device->Release();
    }
    return result;
}

bool render_endpoint_is_active(IMMDeviceEnumerator* enumerator, const std::wstring& id) {
    if (enumerator == nullptr || id.empty()) return false;
    IMMDevice* device = nullptr;
    DWORD state = 0;
    const HRESULT result = enumerator->GetDevice(id.c_str(), &device);
    const bool active = SUCCEEDED(result) && device != nullptr
        && SUCCEEDED(device->GetState(&state))
        && (state & DEVICE_STATE_ACTIVE) != 0;
    if (device != nullptr) device->Release();
    return active;
}

DWORD WINAPI audio_recovery_worker(void*) noexcept {
    const HRESULT com_result = CoInitializeEx(nullptr, COINIT_MULTITHREADED);
    IMMDeviceEnumerator* enumerator = nullptr;
    const HRESULT create_result = CoCreateInstance(
        __uuidof(MMDeviceEnumerator), nullptr, CLSCTX_INPROC_SERVER,
        __uuidof(IMMDeviceEnumerator), reinterpret_cast<void**>(&enumerator));
    if (FAILED(create_result) || enumerator == nullptr) {
        write_log(kLogWarning, "SCBL_AUDIO_RECOVERY monitor=false reason=endpoint-enumerator");
        if (SUCCEEDED(com_result)) CoUninitialize();
        return 0;
    }

    const std::wstring original_endpoint_id = default_render_endpoint_id(enumerator);
    bool original_endpoint_invalidated = false;
    std::wstring baseline = default_render_endpoint_key(enumerator);
    std::wstring observed = baseline;
    UINT32 stable_samples = 0;
    bool change_observed = false;
    write_log(kLogInfo, "SCBL_AUDIO_RECOVERY monitor=true mode=default-render-poll");
    while (!g_audio_recovery_stop.load(std::memory_order_acquire)) {
        Sleep(250);
        if (!original_endpoint_invalidated
            && !render_endpoint_is_active(enumerator, original_endpoint_id)) {
            original_endpoint_invalidated = true;
            native_xaudio27_relay_original_endpoint_invalidated();
        }
        if (g_audio_recovery_waiting_for_game_initialize.load(std::memory_order_acquire)) {
            const ULONGLONG elapsed = GetTickCount64()
                - g_audio_recovery_shutdown_tick.load(std::memory_order_acquire);
            if (elapsed >= 5000) {
                g_audio_recovery_waiting_for_game_initialize.store(false, std::memory_order_release);
                g_audio_recovery_stop.store(true, std::memory_order_release);
                write_log(kLogWarning,
                    "SCBL_AUDIO_RECOVERY reset=false reason=game-lifecycle-initialize-timeout");
                continue;
            }
        }
        const std::wstring current = default_render_endpoint_key(enumerator);
        if (current.empty()) continue;
        if (current != observed) {
            observed = current;
            stable_samples = 0;
            change_observed = observed != baseline;
            continue;
        }
        if (!change_observed || observed == baseline) continue;
        if (++stable_samples < 3) continue;
        baseline = observed;
        change_observed = false;
        stable_samples = 0;
        write_log(kLogInfo,
            "SCBL_AUDIO_NATIVE_TRACE endpoint-change=true behavior=observe-only");
        const std::wstring endpoint_id = default_render_endpoint_id(enumerator);
        if (!endpoint_id.empty()) native_xaudio27_relay_endpoint_changed(endpoint_id.c_str());
    }
    enumerator->Release();
    if (SUCCEEDED(com_result)) CoUninitialize();
    return 0;
}
#endif

const char* kind_name(ObjectKind kind) noexcept {
    switch (kind) {
    case ObjectKind::Engine: return "engine";
    case ObjectKind::Source: return "source";
    case ObjectKind::Submix: return "submix";
    case ObjectKind::Master: return "master";
    }
    return "unknown";
}

const char* method_name(ObjectKind kind, UINT32 slot) noexcept {
    static constexpr const char* engine_names[kEngineSlots] = {
        "QueryInterface", "AddRef", "Release", "GetDeviceCount", "GetDeviceDetails", "Initialize",
        "RegisterForCallbacks", "UnregisterForCallbacks", "CreateSourceVoice", "CreateSubmixVoice",
        "CreateMasteringVoice", "StartEngine", "StopEngine", "CommitChanges", "GetPerformanceData",
        "SetDebugConfiguration"
    };
    static constexpr const char* voice_names[kVoiceSlots] = {
        "GetVoiceDetails", "SetOutputVoices", "SetEffectChain", "EnableEffect", "DisableEffect",
        "GetEffectState", "SetEffectParameters", "GetEffectParameters", "SetFilterParameters",
        "GetFilterParameters", "SetOutputFilterParameters", "GetOutputFilterParameters", "SetVolume",
        "GetVolume", "SetChannelVolumes", "GetChannelVolumes", "SetOutputMatrix", "GetOutputMatrix",
        "DestroyVoice", "Start", "Stop", "SubmitSourceBuffer", "FlushSourceBuffers", "Discontinuity",
        "ExitLoop", "GetState", "SetFrequencyRatio", "GetFrequencyRatio", "SetSourceSampleRate"
    };
    if (kind == ObjectKind::Engine) return slot < kEngineSlots ? engine_names[slot] : "unknown";
    return slot < kVoiceSlots ? voice_names[slot] : "unknown";
}

void write_log(UINT32 level, const char* message) noexcept {
    if (g_log != nullptr && message != nullptr) g_log(level, message);
}

UINT32 caller_offset(void* return_address) noexcept {
    const auto base = reinterpret_cast<uintptr_t>(GetModuleHandleW(nullptr));
    const auto caller = reinterpret_cast<uintptr_t>(return_address);
    if (base == 0 || caller < base || caller - base > UINT32_MAX) return 0;
    return static_cast<UINT32>(caller - base);
}

void trace_call(ObjectKind kind, UINT32 slot) noexcept {
    const UINT32 kind_index = static_cast<UINT32>(kind);
    g_counts[kind_index][slot].fetch_add(1, std::memory_order_relaxed);
    const UINT32 bit = UINT32{1} << slot;
    const UINT32 old = g_seen_masks[kind_index].fetch_or(bit, std::memory_order_relaxed);
    if ((old & bit) == 0) {
        char message[160]{};
        std::snprintf(message, sizeof(message),
            "SCBL_AUDIO_NATIVE_TRACE first=true kind=%s method=%s slot=%u",
            kind_name(kind), method_name(kind, slot), slot);
        write_log(kLogInfo, message);
    }
}

bool snapshot_entry(void* object, void*** original, ObjectKind* kind) noexcept {
    bool found = false;
    AcquireSRWLockShared(&g_entries_lock);
    for (const auto& entry : g_entries) {
        if (entry.active && entry.object == object) {
            *original = entry.original;
            *kind = entry.kind;
            found = true;
            break;
        }
    }
    ReleaseSRWLockShared(&g_entries_lock);
    return found;
}

template<typename Fn>
Fn original_function(void* object, UINT32 slot, ObjectKind* kind = nullptr) noexcept {
    void** original = nullptr;
    ObjectKind found_kind = ObjectKind::Engine;
    if (!snapshot_entry(object, &original, &found_kind) || original == nullptr) return nullptr;
    if (kind != nullptr) *kind = found_kind;
    return reinterpret_cast<Fn>(original[slot]);
}

void remove_entry(void* object, bool restore_vtable) noexcept {
    AcquireSRWLockExclusive(&g_entries_lock);
    for (auto& entry : g_entries) {
        if (!entry.active || entry.object != object) continue;
        if (restore_vtable) {
            InterlockedExchangePointer(reinterpret_cast<void* volatile*>(object), entry.original);
        }
        entry.active = false;
        entry.object = nullptr;
        entry.original = nullptr;
        entry.slots = 0;
        break;
    }
    ReleaseSRWLockExclusive(&g_entries_lock);
}

HRESULT STDMETHODCALLTYPE engine_query_interface(IXAudio2*, REFIID, void**);
ULONG STDMETHODCALLTYPE engine_add_ref(IXAudio2*);
ULONG STDMETHODCALLTYPE engine_release(IXAudio2*);
HRESULT STDMETHODCALLTYPE engine_get_device_count(IXAudio2*, UINT32*);
HRESULT STDMETHODCALLTYPE engine_get_device_details(IXAudio2*, UINT32, XAUDIO2_DEVICE_DETAILS*);
HRESULT STDMETHODCALLTYPE engine_initialize(IXAudio2*, UINT32, XAUDIO2_PROCESSOR);
HRESULT STDMETHODCALLTYPE engine_register_callbacks(IXAudio2*, IXAudio2EngineCallback*);
void STDMETHODCALLTYPE engine_unregister_callbacks(IXAudio2*, IXAudio2EngineCallback*);
HRESULT STDMETHODCALLTYPE engine_create_source(IXAudio2*, IXAudio2SourceVoice**, const WAVEFORMATEX*, UINT32, float, IXAudio2VoiceCallback*, const XAUDIO2_VOICE_SENDS*, const XAUDIO2_EFFECT_CHAIN*);
HRESULT STDMETHODCALLTYPE engine_create_submix(IXAudio2*, IXAudio2SubmixVoice**, UINT32, UINT32, UINT32, UINT32, const XAUDIO2_VOICE_SENDS*, const XAUDIO2_EFFECT_CHAIN*);
HRESULT STDMETHODCALLTYPE engine_create_master(IXAudio2*, IXAudio2MasteringVoice**, UINT32, UINT32, UINT32, UINT32, const XAUDIO2_EFFECT_CHAIN*);
HRESULT STDMETHODCALLTYPE engine_start(IXAudio2*);
void STDMETHODCALLTYPE engine_stop(IXAudio2*);
HRESULT STDMETHODCALLTYPE engine_commit(IXAudio2*, UINT32);
void STDMETHODCALLTYPE engine_performance(IXAudio2*, XAUDIO2_PERFORMANCE_DATA*);
void STDMETHODCALLTYPE engine_debug(IXAudio2*, const XAUDIO2_DEBUG_CONFIGURATION*, void*);

void STDMETHODCALLTYPE voice_details(IXAudio2Voice*, XAUDIO2_VOICE_DETAILS*);
HRESULT STDMETHODCALLTYPE voice_set_outputs(IXAudio2Voice*, const XAUDIO2_VOICE_SENDS*);
HRESULT STDMETHODCALLTYPE voice_set_effect_chain(IXAudio2Voice*, const XAUDIO2_EFFECT_CHAIN*);
HRESULT STDMETHODCALLTYPE voice_enable_effect(IXAudio2Voice*, UINT32, UINT32);
HRESULT STDMETHODCALLTYPE voice_disable_effect(IXAudio2Voice*, UINT32, UINT32);
void STDMETHODCALLTYPE voice_get_effect_state(IXAudio2Voice*, UINT32, BOOL*);
HRESULT STDMETHODCALLTYPE voice_set_effect_parameters(IXAudio2Voice*, UINT32, const void*, UINT32, UINT32);
HRESULT STDMETHODCALLTYPE voice_get_effect_parameters(IXAudio2Voice*, UINT32, void*, UINT32);
HRESULT STDMETHODCALLTYPE voice_set_filter(IXAudio2Voice*, const XAUDIO2_FILTER_PARAMETERS*, UINT32);
void STDMETHODCALLTYPE voice_get_filter(IXAudio2Voice*, XAUDIO2_FILTER_PARAMETERS*);
HRESULT STDMETHODCALLTYPE voice_set_output_filter(IXAudio2Voice*, IXAudio2Voice*, const XAUDIO2_FILTER_PARAMETERS*, UINT32);
void STDMETHODCALLTYPE voice_get_output_filter(IXAudio2Voice*, IXAudio2Voice*, XAUDIO2_FILTER_PARAMETERS*);
HRESULT STDMETHODCALLTYPE voice_set_volume(IXAudio2Voice*, float, UINT32);
void STDMETHODCALLTYPE voice_get_volume(IXAudio2Voice*, float*);
HRESULT STDMETHODCALLTYPE voice_set_channel_volumes(IXAudio2Voice*, UINT32, const float*, UINT32);
void STDMETHODCALLTYPE voice_get_channel_volumes(IXAudio2Voice*, UINT32, float*);
HRESULT STDMETHODCALLTYPE voice_set_output_matrix(IXAudio2Voice*, IXAudio2Voice*, UINT32, UINT32, const float*, UINT32);
void STDMETHODCALLTYPE voice_get_output_matrix(IXAudio2Voice*, IXAudio2Voice*, UINT32, UINT32, float*);
void STDMETHODCALLTYPE voice_destroy(IXAudio2Voice*);
HRESULT STDMETHODCALLTYPE source_start(IXAudio2SourceVoice*, UINT32, UINT32);
HRESULT STDMETHODCALLTYPE source_stop(IXAudio2SourceVoice*, UINT32, UINT32);
HRESULT STDMETHODCALLTYPE source_submit(IXAudio2SourceVoice*, const XAUDIO2_BUFFER*, const XAUDIO2_BUFFER_WMA*);
HRESULT STDMETHODCALLTYPE source_flush(IXAudio2SourceVoice*);
HRESULT STDMETHODCALLTYPE source_discontinuity(IXAudio2SourceVoice*);
HRESULT STDMETHODCALLTYPE source_exit_loop(IXAudio2SourceVoice*, UINT32);
void STDMETHODCALLTYPE source_get_state(IXAudio2SourceVoice*, XAUDIO2_VOICE_STATE*);
HRESULT STDMETHODCALLTYPE source_set_ratio(IXAudio2SourceVoice*, float, UINT32);
void STDMETHODCALLTYPE source_get_ratio(IXAudio2SourceVoice*, float*);
HRESULT STDMETHODCALLTYPE source_set_rate(IXAudio2SourceVoice*, UINT32);

bool patch_object(void* object, ObjectKind kind) noexcept {
    if (object == nullptr) return false;
    void** original = *reinterpret_cast<void***>(object);
    if (original == nullptr) return false;

    AcquireSRWLockExclusive(&g_entries_lock);
    for (const auto& entry : g_entries) {
        if (entry.active && entry.object == object) {
            ReleaseSRWLockExclusive(&g_entries_lock);
            return true;
        }
    }
    TraceEntry* target = nullptr;
    for (auto& entry : g_entries) {
        if (!entry.active) {
            target = &entry;
            break;
        }
    }
    if (target == nullptr) {
        ReleaseSRWLockExclusive(&g_entries_lock);
        write_log(kLogWarning, "SCBL_AUDIO_NATIVE_TRACE available=false reason=object-capacity");
        return false;
    }

    target->object = object;
    target->original = original;
    target->kind = kind;
    target->slots = kind == ObjectKind::Engine ? kEngineSlots : (kind == ObjectKind::Source ? kVoiceSlots : 19);
    std::memcpy(target->traced.data(), original, target->slots * sizeof(void*));

    if (kind == ObjectKind::Engine) {
        target->traced[0] = reinterpret_cast<void*>(engine_query_interface);
        target->traced[1] = reinterpret_cast<void*>(engine_add_ref);
        target->traced[2] = reinterpret_cast<void*>(engine_release);
        target->traced[3] = reinterpret_cast<void*>(engine_get_device_count);
        target->traced[4] = reinterpret_cast<void*>(engine_get_device_details);
        target->traced[5] = reinterpret_cast<void*>(engine_initialize);
        target->traced[6] = reinterpret_cast<void*>(engine_register_callbacks);
        target->traced[7] = reinterpret_cast<void*>(engine_unregister_callbacks);
        target->traced[8] = reinterpret_cast<void*>(engine_create_source);
        target->traced[9] = reinterpret_cast<void*>(engine_create_submix);
        target->traced[10] = reinterpret_cast<void*>(engine_create_master);
        target->traced[11] = reinterpret_cast<void*>(engine_start);
        target->traced[12] = reinterpret_cast<void*>(engine_stop);
        target->traced[13] = reinterpret_cast<void*>(engine_commit);
        target->traced[14] = reinterpret_cast<void*>(engine_performance);
        target->traced[15] = reinterpret_cast<void*>(engine_debug);
    } else {
        target->traced[0] = reinterpret_cast<void*>(voice_details);
        target->traced[1] = reinterpret_cast<void*>(voice_set_outputs);
        target->traced[2] = reinterpret_cast<void*>(voice_set_effect_chain);
        target->traced[3] = reinterpret_cast<void*>(voice_enable_effect);
        target->traced[4] = reinterpret_cast<void*>(voice_disable_effect);
        target->traced[5] = reinterpret_cast<void*>(voice_get_effect_state);
        target->traced[6] = reinterpret_cast<void*>(voice_set_effect_parameters);
        target->traced[7] = reinterpret_cast<void*>(voice_get_effect_parameters);
        target->traced[8] = reinterpret_cast<void*>(voice_set_filter);
        target->traced[9] = reinterpret_cast<void*>(voice_get_filter);
        target->traced[10] = reinterpret_cast<void*>(voice_set_output_filter);
        target->traced[11] = reinterpret_cast<void*>(voice_get_output_filter);
        target->traced[12] = reinterpret_cast<void*>(voice_set_volume);
        target->traced[13] = reinterpret_cast<void*>(voice_get_volume);
        target->traced[14] = reinterpret_cast<void*>(voice_set_channel_volumes);
        target->traced[15] = reinterpret_cast<void*>(voice_get_channel_volumes);
        target->traced[16] = reinterpret_cast<void*>(voice_set_output_matrix);
        target->traced[17] = reinterpret_cast<void*>(voice_get_output_matrix);
        target->traced[18] = reinterpret_cast<void*>(voice_destroy);
        if (kind == ObjectKind::Source) {
            target->traced[19] = reinterpret_cast<void*>(source_start);
            target->traced[20] = reinterpret_cast<void*>(source_stop);
            target->traced[21] = reinterpret_cast<void*>(source_submit);
            target->traced[22] = reinterpret_cast<void*>(source_flush);
            target->traced[23] = reinterpret_cast<void*>(source_discontinuity);
            target->traced[24] = reinterpret_cast<void*>(source_exit_loop);
            target->traced[25] = reinterpret_cast<void*>(source_get_state);
            target->traced[26] = reinterpret_cast<void*>(source_set_ratio);
            target->traced[27] = reinterpret_cast<void*>(source_get_ratio);
            target->traced[28] = reinterpret_cast<void*>(source_set_rate);
        }
    }
    target->active = true;
    InterlockedExchangePointer(reinterpret_cast<void* volatile*>(object), target->traced.data());
    ReleaseSRWLockExclusive(&g_entries_lock);
    return true;
}

template<typename Fn, typename Self, typename... Args>
auto call_original(Self* object, UINT32 slot, Args... args) noexcept
    -> decltype(reinterpret_cast<Fn>(nullptr)(object, args...)) {
    ObjectKind kind{};
    Fn fn = original_function<Fn>(object, slot, &kind);
    trace_call(kind, slot);
    return fn(object, args...);
}

#define SCBL_ENGINE_CALL(slot, type, self, ...) call_original<type>(self, slot, __VA_ARGS__)
#define SCBL_VOICE_CALL(slot, type, self, ...) call_original<type>(self, slot, __VA_ARGS__)

HRESULT STDMETHODCALLTYPE engine_query_interface(IXAudio2* self, REFIID iid, void** out) {
    using Fn = HRESULT(STDMETHODCALLTYPE*)(IXAudio2*, REFIID, void**);
    return SCBL_ENGINE_CALL(0, Fn, self, iid, out);
}
ULONG STDMETHODCALLTYPE engine_add_ref(IXAudio2* self) {
    using Fn = ULONG(STDMETHODCALLTYPE*)(IXAudio2*);
    ObjectKind kind{}; Fn fn = original_function<Fn>(self, 1, &kind); trace_call(kind, 1); return fn(self);
}
ULONG STDMETHODCALLTYPE engine_release(IXAudio2* self) {
    using Fn = ULONG(STDMETHODCALLTYPE*)(IXAudio2*);
    ObjectKind kind{}; Fn fn = original_function<Fn>(self, 2, &kind); trace_call(kind, 2);
    const ULONG result = fn(self);
    if (result == 0) remove_entry(self, false);
    return result;
}
HRESULT STDMETHODCALLTYPE engine_get_device_count(IXAudio2* self, UINT32* count) {
    using Fn = HRESULT(STDMETHODCALLTYPE*)(IXAudio2*, UINT32*);
    return SCBL_ENGINE_CALL(3, Fn, self, count);
}
HRESULT STDMETHODCALLTYPE engine_get_device_details(IXAudio2* self, UINT32 index, XAUDIO2_DEVICE_DETAILS* details) {
    using Fn = HRESULT(STDMETHODCALLTYPE*)(IXAudio2*, UINT32, XAUDIO2_DEVICE_DETAILS*);
    return SCBL_ENGINE_CALL(4, Fn, self, index, details);
}
HRESULT STDMETHODCALLTYPE engine_initialize(IXAudio2* self, UINT32 flags, XAUDIO2_PROCESSOR processor) {
    const UINT32 caller = caller_offset(_ReturnAddress());
    using Fn = HRESULT(STDMETHODCALLTYPE*)(IXAudio2*, UINT32, XAUDIO2_PROCESSOR);
    const HRESULT result = SCBL_ENGINE_CALL(5, Fn, self, flags, processor);
    char message[160]{};
    std::snprintf(message, sizeof(message), "SCBL_AUDIO_NATIVE_TRACE initialize hr=0x%08lX flags=%u processor=%u caller=0x%08X",
        static_cast<unsigned long>(result), flags, processor, caller);
    write_log(SUCCEEDED(result) ? kLogInfo : kLogWarning, message);
    return result;
}
HRESULT STDMETHODCALLTYPE engine_register_callbacks(IXAudio2* self, IXAudio2EngineCallback* callback) {
    const UINT32 caller = caller_offset(_ReturnAddress());
    using Fn = HRESULT(STDMETHODCALLTYPE*)(IXAudio2*, IXAudio2EngineCallback*);
    const HRESULT result = SCBL_ENGINE_CALL(6, Fn, self, callback);
    if (SUCCEEDED(result)) g_engine_callback.store(callback, std::memory_order_release);
    void* critical_error = nullptr;
    if (callback != nullptr) {
        auto** callback_vtable = *reinterpret_cast<void***>(callback);
        if (callback_vtable != nullptr) critical_error = callback_vtable[2];
    }
    char message[208]{};
    std::snprintf(message, sizeof(message),
        "SCBL_AUDIO_NATIVE_TRACE register-callbacks hr=0x%08lX caller=0x%08X critical-error=0x%08X",
        static_cast<unsigned long>(result), caller, caller_offset(critical_error));
    write_log(SUCCEEDED(result) ? kLogInfo : kLogWarning, message);
    return result;
}
void STDMETHODCALLTYPE engine_unregister_callbacks(IXAudio2* self, IXAudio2EngineCallback* callback) {
    using Fn = void(STDMETHODCALLTYPE*)(IXAudio2*, IXAudio2EngineCallback*);
    SCBL_ENGINE_CALL(7, Fn, self, callback);
    IXAudio2EngineCallback* expected = callback;
    g_engine_callback.compare_exchange_strong(expected, nullptr, std::memory_order_acq_rel);
}
HRESULT STDMETHODCALLTYPE engine_create_source(IXAudio2* self, IXAudio2SourceVoice** output, const WAVEFORMATEX* format,
    UINT32 flags, float ratio, IXAudio2VoiceCallback* callback, const XAUDIO2_VOICE_SENDS* sends,
    const XAUDIO2_EFFECT_CHAIN* effects) {
    using Fn = HRESULT(STDMETHODCALLTYPE*)(IXAudio2*, IXAudio2SourceVoice**, const WAVEFORMATEX*, UINT32, float, IXAudio2VoiceCallback*, const XAUDIO2_VOICE_SENDS*, const XAUDIO2_EFFECT_CHAIN*);
    const HRESULT result = SCBL_ENGINE_CALL(8, Fn, self, output, format, flags, ratio, callback, sends, effects);
    if (SUCCEEDED(result) && output != nullptr && *output != nullptr) {
        g_created_sources_total.fetch_add(1, std::memory_order_relaxed);
    }
    char message[256]{};
    std::snprintf(message, sizeof(message),
        "SCBL_AUDIO_NATIVE_TRACE create=source hr=0x%08lX channels=%u rate=%lu flags=%u sends=%u effects=%u callback=%u created_total=%u",
        static_cast<unsigned long>(result), format != nullptr ? format->nChannels : 0,
        format != nullptr ? static_cast<unsigned long>(format->nSamplesPerSec) : 0, flags,
        sends != nullptr ? sends->SendCount : 0, effects != nullptr ? effects->EffectCount : 0,
        callback != nullptr ? 1U : 0U, g_created_sources_total.load(std::memory_order_relaxed));
    write_log(SUCCEEDED(result) ? kLogInfo : kLogWarning, message);
    return result;
}
HRESULT STDMETHODCALLTYPE engine_create_submix(IXAudio2* self, IXAudio2SubmixVoice** output, UINT32 channels, UINT32 rate,
    UINT32 flags, UINT32 stage, const XAUDIO2_VOICE_SENDS* sends, const XAUDIO2_EFFECT_CHAIN* effects) {
    using Fn = HRESULT(STDMETHODCALLTYPE*)(IXAudio2*, IXAudio2SubmixVoice**, UINT32, UINT32, UINT32, UINT32, const XAUDIO2_VOICE_SENDS*, const XAUDIO2_EFFECT_CHAIN*);
    const HRESULT result = SCBL_ENGINE_CALL(9, Fn, self, output, channels, rate, flags, stage, sends, effects);
    if (SUCCEEDED(result) && output != nullptr && *output != nullptr) {
        g_created_submixes_total.fetch_add(1, std::memory_order_relaxed);
    }
    char message[224]{};
    std::snprintf(message, sizeof(message),
        "SCBL_AUDIO_NATIVE_TRACE create=submix hr=0x%08lX channels=%u rate=%u flags=%u stage=%u sends=%u effects=%u created_total=%u",
        static_cast<unsigned long>(result), channels, rate, flags, stage, sends != nullptr ? sends->SendCount : 0,
        effects != nullptr ? effects->EffectCount : 0, g_created_submixes_total.load(std::memory_order_relaxed));
    write_log(SUCCEEDED(result) ? kLogInfo : kLogWarning, message);
    return result;
}
HRESULT STDMETHODCALLTYPE create_native_master(IXAudio2* self, IXAudio2MasteringVoice** output,
    UINT32 channels, UINT32 rate, UINT32 flags, UINT32 device, const XAUDIO2_EFFECT_CHAIN* effects) {
    return SCBL_ENGINE_CALL(10, CreateMasterVoiceFn, self, output, channels, rate, flags, device, effects);
}
HRESULT STDMETHODCALLTYPE engine_create_master(IXAudio2* self, IXAudio2MasteringVoice** output, UINT32 channels, UINT32 rate,
    UINT32 flags, UINT32 device, const XAUDIO2_EFFECT_CHAIN* effects) {
    const UINT32 caller = caller_offset(_ReturnAddress());
    const auto created = create_master_output(self, output, {channels, rate, flags, device, effects},
        create_native_master, g_log);
    const HRESULT result = created.status;
    const UINT32 anchor_device = created.device;
    if (SUCCEEDED(result) && output != nullptr && *output != nullptr) {
        g_active_masters.fetch_add(1, std::memory_order_relaxed);
        patch_object(*output, ObjectKind::Master);
    }
    char message[208]{};
    std::snprintf(message, sizeof(message),
        "SCBL_AUDIO_NATIVE_TRACE create=master hr=0x%08lX channels=%u rate=%u flags=%u device=%u anchor=%u effects=%u active=%u caller=0x%08X",
        static_cast<unsigned long>(result), channels, rate, flags, device, anchor_device,
        effects != nullptr ? effects->EffectCount : 0,
        g_active_masters.load(std::memory_order_relaxed), caller);
    write_log(SUCCEEDED(result) ? kLogInfo : kLogWarning, message);
    return result;
}
HRESULT STDMETHODCALLTYPE engine_start(IXAudio2* self) { using Fn = HRESULT(STDMETHODCALLTYPE*)(IXAudio2*); ObjectKind k{}; Fn f=original_function<Fn>(self,11,&k); trace_call(k,11); return f(self); }
void STDMETHODCALLTYPE engine_stop(IXAudio2* self) { using Fn = void(STDMETHODCALLTYPE*)(IXAudio2*); ObjectKind k{}; Fn f=original_function<Fn>(self,12,&k); trace_call(k,12); f(self); }
HRESULT STDMETHODCALLTYPE engine_commit(IXAudio2* self, UINT32 operation) { using Fn=HRESULT(STDMETHODCALLTYPE*)(IXAudio2*,UINT32); return SCBL_ENGINE_CALL(13,Fn,self,operation); }
void STDMETHODCALLTYPE engine_performance(IXAudio2* self, XAUDIO2_PERFORMANCE_DATA* data) { using Fn=void(STDMETHODCALLTYPE*)(IXAudio2*,XAUDIO2_PERFORMANCE_DATA*); SCBL_ENGINE_CALL(14,Fn,self,data); }
void STDMETHODCALLTYPE engine_debug(IXAudio2* self, const XAUDIO2_DEBUG_CONFIGURATION* config, void* reserved) { using Fn=void(STDMETHODCALLTYPE*)(IXAudio2*,const XAUDIO2_DEBUG_CONFIGURATION*,void*); SCBL_ENGINE_CALL(15,Fn,self,config,reserved); }

void STDMETHODCALLTYPE voice_details(IXAudio2Voice* self, XAUDIO2_VOICE_DETAILS* out) { using Fn=void(STDMETHODCALLTYPE*)(IXAudio2Voice*,XAUDIO2_VOICE_DETAILS*); SCBL_VOICE_CALL(0,Fn,self,out); }
HRESULT STDMETHODCALLTYPE voice_set_outputs(IXAudio2Voice* self, const XAUDIO2_VOICE_SENDS* sends) { using Fn=HRESULT(STDMETHODCALLTYPE*)(IXAudio2Voice*,const XAUDIO2_VOICE_SENDS*); return SCBL_VOICE_CALL(1,Fn,self,sends); }
HRESULT STDMETHODCALLTYPE voice_set_effect_chain(IXAudio2Voice* self, const XAUDIO2_EFFECT_CHAIN* effects) { using Fn=HRESULT(STDMETHODCALLTYPE*)(IXAudio2Voice*,const XAUDIO2_EFFECT_CHAIN*); return SCBL_VOICE_CALL(2,Fn,self,effects); }
HRESULT STDMETHODCALLTYPE voice_enable_effect(IXAudio2Voice* self, UINT32 index, UINT32 op) { using Fn=HRESULT(STDMETHODCALLTYPE*)(IXAudio2Voice*,UINT32,UINT32); return SCBL_VOICE_CALL(3,Fn,self,index,op); }
HRESULT STDMETHODCALLTYPE voice_disable_effect(IXAudio2Voice* self, UINT32 index, UINT32 op) { using Fn=HRESULT(STDMETHODCALLTYPE*)(IXAudio2Voice*,UINT32,UINT32); return SCBL_VOICE_CALL(4,Fn,self,index,op); }
void STDMETHODCALLTYPE voice_get_effect_state(IXAudio2Voice* self, UINT32 index, BOOL* enabled) { using Fn=void(STDMETHODCALLTYPE*)(IXAudio2Voice*,UINT32,BOOL*); SCBL_VOICE_CALL(5,Fn,self,index,enabled); }
HRESULT STDMETHODCALLTYPE voice_set_effect_parameters(IXAudio2Voice* self, UINT32 index, const void* data, UINT32 size, UINT32 op) { using Fn=HRESULT(STDMETHODCALLTYPE*)(IXAudio2Voice*,UINT32,const void*,UINT32,UINT32); return SCBL_VOICE_CALL(6,Fn,self,index,data,size,op); }
HRESULT STDMETHODCALLTYPE voice_get_effect_parameters(IXAudio2Voice* self, UINT32 index, void* data, UINT32 size) { using Fn=HRESULT(STDMETHODCALLTYPE*)(IXAudio2Voice*,UINT32,void*,UINT32); return SCBL_VOICE_CALL(7,Fn,self,index,data,size); }
HRESULT STDMETHODCALLTYPE voice_set_filter(IXAudio2Voice* self, const XAUDIO2_FILTER_PARAMETERS* p, UINT32 op) { using Fn=HRESULT(STDMETHODCALLTYPE*)(IXAudio2Voice*,const XAUDIO2_FILTER_PARAMETERS*,UINT32); return SCBL_VOICE_CALL(8,Fn,self,p,op); }
void STDMETHODCALLTYPE voice_get_filter(IXAudio2Voice* self, XAUDIO2_FILTER_PARAMETERS* p) { using Fn=void(STDMETHODCALLTYPE*)(IXAudio2Voice*,XAUDIO2_FILTER_PARAMETERS*); SCBL_VOICE_CALL(9,Fn,self,p); }
HRESULT STDMETHODCALLTYPE voice_set_output_filter(IXAudio2Voice* self, IXAudio2Voice* dst, const XAUDIO2_FILTER_PARAMETERS* p, UINT32 op) { using Fn=HRESULT(STDMETHODCALLTYPE*)(IXAudio2Voice*,IXAudio2Voice*,const XAUDIO2_FILTER_PARAMETERS*,UINT32); return SCBL_VOICE_CALL(10,Fn,self,dst,p,op); }
void STDMETHODCALLTYPE voice_get_output_filter(IXAudio2Voice* self, IXAudio2Voice* dst, XAUDIO2_FILTER_PARAMETERS* p) { using Fn=void(STDMETHODCALLTYPE*)(IXAudio2Voice*,IXAudio2Voice*,XAUDIO2_FILTER_PARAMETERS*); SCBL_VOICE_CALL(11,Fn,self,dst,p); }
HRESULT STDMETHODCALLTYPE voice_set_volume(IXAudio2Voice* self, float v, UINT32 op) {
    using Fn=HRESULT(STDMETHODCALLTYPE*)(IXAudio2Voice*,float,UINT32);
    ObjectKind kind{};
    Fn fn=original_function<Fn>(self,12,&kind);
    trace_call(kind,12);
    float forwarded=v;
    native_xaudio27_relay_adjust_master_volume(self,v,&forwarded);
    return fn(self,forwarded,op);
}
void STDMETHODCALLTYPE voice_get_volume(IXAudio2Voice* self, float* v) {
    using Fn=void(STDMETHODCALLTYPE*)(IXAudio2Voice*,float*);
    ObjectKind kind{};
    Fn fn=original_function<Fn>(self,13,&kind);
    trace_call(kind,13);
    fn(self,v);
    native_xaudio27_relay_override_master_volume(self,v);
}
HRESULT STDMETHODCALLTYPE voice_set_channel_volumes(IXAudio2Voice* self, UINT32 n, const float* v, UINT32 op) { using Fn=HRESULT(STDMETHODCALLTYPE*)(IXAudio2Voice*,UINT32,const float*,UINT32); return SCBL_VOICE_CALL(14,Fn,self,n,v,op); }
void STDMETHODCALLTYPE voice_get_channel_volumes(IXAudio2Voice* self, UINT32 n, float* v) { using Fn=void(STDMETHODCALLTYPE*)(IXAudio2Voice*,UINT32,float*); SCBL_VOICE_CALL(15,Fn,self,n,v); }
HRESULT STDMETHODCALLTYPE voice_set_output_matrix(IXAudio2Voice* self, IXAudio2Voice* dst, UINT32 srcn, UINT32 dstn, const float* m, UINT32 op) { using Fn=HRESULT(STDMETHODCALLTYPE*)(IXAudio2Voice*,IXAudio2Voice*,UINT32,UINT32,const float*,UINT32); return SCBL_VOICE_CALL(16,Fn,self,dst,srcn,dstn,m,op); }
void STDMETHODCALLTYPE voice_get_output_matrix(IXAudio2Voice* self, IXAudio2Voice* dst, UINT32 srcn, UINT32 dstn, float* m) { using Fn=void(STDMETHODCALLTYPE*)(IXAudio2Voice*,IXAudio2Voice*,UINT32,UINT32,float*); SCBL_VOICE_CALL(17,Fn,self,dst,srcn,dstn,m); }
void STDMETHODCALLTYPE voice_destroy(IXAudio2Voice* self) {
    using Fn=void(STDMETHODCALLTYPE*)(IXAudio2Voice*); ObjectKind kind{}; Fn fn=original_function<Fn>(self,18,&kind); trace_call(kind,18);
    if (kind == ObjectKind::Master) {
        g_active_masters.fetch_sub(1,std::memory_order_relaxed);
        native_xaudio27_relay_master_destroying(self);
    }
    remove_entry(self,true); fn(self);
}
HRESULT STDMETHODCALLTYPE source_start(IXAudio2SourceVoice* self, UINT32 flags, UINT32 op) { using Fn=HRESULT(STDMETHODCALLTYPE*)(IXAudio2SourceVoice*,UINT32,UINT32); return SCBL_VOICE_CALL(19,Fn,self,flags,op); }
HRESULT STDMETHODCALLTYPE source_stop(IXAudio2SourceVoice* self, UINT32 flags, UINT32 op) { using Fn=HRESULT(STDMETHODCALLTYPE*)(IXAudio2SourceVoice*,UINT32,UINT32); return SCBL_VOICE_CALL(20,Fn,self,flags,op); }
HRESULT STDMETHODCALLTYPE source_submit(IXAudio2SourceVoice* self, const XAUDIO2_BUFFER* buffer, const XAUDIO2_BUFFER_WMA* wma) { using Fn=HRESULT(STDMETHODCALLTYPE*)(IXAudio2SourceVoice*,const XAUDIO2_BUFFER*,const XAUDIO2_BUFFER_WMA*); return SCBL_VOICE_CALL(21,Fn,self,buffer,wma); }
HRESULT STDMETHODCALLTYPE source_flush(IXAudio2SourceVoice* self) { using Fn=HRESULT(STDMETHODCALLTYPE*)(IXAudio2SourceVoice*); ObjectKind k{}; Fn f=original_function<Fn>(self,22,&k); trace_call(k,22); return f(self); }
HRESULT STDMETHODCALLTYPE source_discontinuity(IXAudio2SourceVoice* self) { using Fn=HRESULT(STDMETHODCALLTYPE*)(IXAudio2SourceVoice*); ObjectKind k{}; Fn f=original_function<Fn>(self,23,&k); trace_call(k,23); return f(self); }
HRESULT STDMETHODCALLTYPE source_exit_loop(IXAudio2SourceVoice* self, UINT32 op) { using Fn=HRESULT(STDMETHODCALLTYPE*)(IXAudio2SourceVoice*,UINT32); return SCBL_VOICE_CALL(24,Fn,self,op); }
void STDMETHODCALLTYPE source_get_state(IXAudio2SourceVoice* self, XAUDIO2_VOICE_STATE* state) { using Fn=void(STDMETHODCALLTYPE*)(IXAudio2SourceVoice*,XAUDIO2_VOICE_STATE*); SCBL_VOICE_CALL(25,Fn,self,state); }
HRESULT STDMETHODCALLTYPE source_set_ratio(IXAudio2SourceVoice* self, float ratio, UINT32 op) { using Fn=HRESULT(STDMETHODCALLTYPE*)(IXAudio2SourceVoice*,float,UINT32); return SCBL_VOICE_CALL(26,Fn,self,ratio,op); }
void STDMETHODCALLTYPE source_get_ratio(IXAudio2SourceVoice* self, float* ratio) { using Fn=void(STDMETHODCALLTYPE*)(IXAudio2SourceVoice*,float*); SCBL_VOICE_CALL(27,Fn,self,ratio); }
HRESULT STDMETHODCALLTYPE source_set_rate(IXAudio2SourceVoice* self, UINT32 rate) { using Fn=HRESULT(STDMETHODCALLTYPE*)(IXAudio2SourceVoice*,UINT32); return SCBL_VOICE_CALL(28,Fn,self,rate); }

#undef SCBL_ENGINE_CALL
#undef SCBL_VOICE_CALL

void** find_main_iat_slot(const char* dll_name, const char* function_name) noexcept {
    auto* base = reinterpret_cast<BYTE*>(GetModuleHandleW(nullptr));
    if (base == nullptr) return nullptr;
    const auto* dos = reinterpret_cast<const IMAGE_DOS_HEADER*>(base);
    if (dos->e_magic != IMAGE_DOS_SIGNATURE) return nullptr;
    const auto* nt = reinterpret_cast<const IMAGE_NT_HEADERS*>(base + dos->e_lfanew);
    if (nt->Signature != IMAGE_NT_SIGNATURE) return nullptr;
    const IMAGE_DATA_DIRECTORY& directory = nt->OptionalHeader.DataDirectory[IMAGE_DIRECTORY_ENTRY_IMPORT];
    if (directory.VirtualAddress == 0 || directory.Size < sizeof(IMAGE_IMPORT_DESCRIPTOR)) return nullptr;
    auto* descriptor = reinterpret_cast<IMAGE_IMPORT_DESCRIPTOR*>(base + directory.VirtualAddress);
    for (; descriptor->Name != 0; ++descriptor) {
        const char* imported_dll = reinterpret_cast<const char*>(base + descriptor->Name);
        if (_stricmp(imported_dll, dll_name) != 0) continue;
        auto* lookup = reinterpret_cast<IMAGE_THUNK_DATA*>(base +
            (descriptor->OriginalFirstThunk != 0 ? descriptor->OriginalFirstThunk : descriptor->FirstThunk));
        auto* iat = reinterpret_cast<IMAGE_THUNK_DATA*>(base + descriptor->FirstThunk);
        for (; lookup->u1.AddressOfData != 0; ++lookup, ++iat) {
            if (IMAGE_SNAP_BY_ORDINAL(lookup->u1.Ordinal)) continue;
            const auto* imported = reinterpret_cast<const IMAGE_IMPORT_BY_NAME*>(base + lookup->u1.AddressOfData);
            if (std::strcmp(reinterpret_cast<const char*>(imported->Name), function_name) == 0) {
                return reinterpret_cast<void**>(&iat->u1.Function);
            }
        }
    }
    return nullptr;
}

bool patch_main_iat_slot(
    void** slot,
    void* replacement,
    const char* failure_reason) noexcept {
    if (slot == nullptr || *slot == nullptr || replacement == nullptr) {
        write_log(kLogWarning, failure_reason);
        return false;
    }
    DWORD old_protection = 0;
    if (!VirtualProtect(slot, sizeof(void*), PAGE_READWRITE, &old_protection)) {
        write_log(kLogWarning, failure_reason);
        return false;
    }
    InterlockedExchangePointer(slot, replacement);
    DWORD ignored = 0;
    VirtualProtect(slot, sizeof(void*), old_protection, &ignored);
    FlushInstructionCache(GetCurrentProcess(), slot, sizeof(void*));
    return true;
}

HRESULT WINAPI traced_co_create_instance(REFCLSID clsid, LPUNKNOWN outer, DWORD context, REFIID iid, LPVOID* output) noexcept {
    const HRESULT result = g_original_co_create != nullptr
        ? g_original_co_create(clsid, outer, context, iid, output)
        : E_UNEXPECTED;
    if (SUCCEEDED(result) && output != nullptr && *output != nullptr && outer == nullptr
        && InlineIsEqualGUID(clsid, scbl_xaudio27_class_id())
        && InlineIsEqualGUID(iid, scbl_xaudio27_interface_id())) {
        if (patch_object(*output, ObjectKind::Engine)) {
            write_log(kLogInfo, "SCBL_AUDIO_NATIVE_TRACE engine-attached=true passthrough=true");
        } else {
            write_log(kLogWarning, "SCBL_AUDIO_NATIVE_TRACE engine-attached=false passthrough=true");
        }
    }
    return result;
}

} // namespace

bool install_native_xaudio27_trace(NativeTraceLogFn log) noexcept {
    if (g_iat_slot != nullptr) return true;
    g_log = log;
    void** slot = find_main_iat_slot("ole32.dll", "CoCreateInstance");
    if (slot == nullptr || *slot == nullptr) {
        write_log(kLogWarning, "SCBL_AUDIO_NATIVE_TRACE available=false reason=co-create-iat-missing");
        g_log = nullptr;
        return false;
    }
    g_original_co_create = reinterpret_cast<CoCreateInstanceFn>(*slot);
    if (!patch_main_iat_slot(
            slot,
            reinterpret_cast<void*>(traced_co_create_instance),
            "SCBL_AUDIO_NATIVE_TRACE available=false reason=iat-protection")) {
        g_original_co_create = nullptr;
        g_log = nullptr;
        return false;
    }
    g_iat_slot = slot;
    g_audio_recovery_stop.store(false, std::memory_order_release);
    g_audio_recovery_thread = CreateThread(nullptr, 0, audio_recovery_worker, nullptr, 0, nullptr);
    if (g_audio_recovery_thread == nullptr) {
        write_log(kLogWarning, "SCBL_AUDIO_NATIVE_TRACE endpoint-monitor=false reason=worker-create");
    } else {
        CloseHandle(g_audio_recovery_thread);
        g_audio_recovery_thread = nullptr;
    }
    write_log(kLogInfo, "SCBL_AUDIO_NATIVE_TRACE installed=true backend=xaudio27 behavior=observe-only");
    return true;
}

void request_native_audio_recovery_stop() noexcept {
    g_audio_recovery_stop.store(true, std::memory_order_release);
    native_xaudio27_relay_request_stop();
}

} // namespace scbl::audio

