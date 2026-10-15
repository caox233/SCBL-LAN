#include "audio_router.hpp"

#include <Windows.h>

#include <atomic>
#include <cstddef>
#include <cstring>
#include <new>

namespace {

constexpr UINT32 kPluginAbiVersion = 1;
constexpr UINT32 kLogInfo = 2;

using HostLogFn = void(__stdcall*)(UINT32, const char*, UINT32);

struct HostApiV1 {
    UINT32 abi_version;
    UINT32 struct_size;
    HostLogFn log;
};

struct PluginConfigV1 {
    UINT32 abi_version;
    UINT32 struct_size;
    UINT64 feature_bits;
    UINT32 world_raycast;
    UINT32 reserved;
    const wchar_t* loadout_sidecar_path;
    UINT32 loadout_sidecar_path_len;
    UINT32 reserved2;
};

std::atomic<HostLogFn> g_host_log{nullptr};
static_assert(std::atomic<HostLogFn>::is_always_lock_free);
scbl::audio::Router* g_router = nullptr;

void forward_log(UINT32 level, const char* message) noexcept {
    const HostLogFn log = g_host_log.load(std::memory_order_acquire);
    if (log == nullptr || message == nullptr) {
        return;
    }
    // Shutdown clears publication without waiting under the loader lock.
    // An in-flight snapshot may finish because the built-in host stays mapped.
    log(level, message, static_cast<UINT32>(std::strlen(message)));
}

} // namespace

#if defined(SCBL_AUDIO_BUILTIN)
#define SCBL_AUDIO_INITIALIZE scbl_audio_builtin_initialize
#define SCBL_AUDIO_SHUTDOWN scbl_audio_builtin_shutdown
#else
#define SCBL_AUDIO_INITIALIZE scbl_plugin_initialize
#define SCBL_AUDIO_SHUTDOWN scbl_plugin_shutdown
#endif

extern "C" int __stdcall SCBL_AUDIO_INITIALIZE(const HostApiV1* host, const PluginConfigV1* config) noexcept {
    if (host == nullptr || config == nullptr) {
        return -1;
    }
    if (host->abi_version != kPluginAbiVersion || host->struct_size < sizeof(HostApiV1)
        || config->abi_version != kPluginAbiVersion || config->struct_size < sizeof(PluginConfigV1)) {
        return -2;
    }
    if (g_router != nullptr) {
        return 0;
    }
    g_host_log.store(host->log, std::memory_order_release);
    auto* router = new (std::nothrow) scbl::audio::Router();
    if (router == nullptr || !router->start(forward_log)) {
        delete router;
        g_host_log.store(nullptr, std::memory_order_release);
        return -3;
    }
    g_router = router;
    forward_log(kLogInfo, "SCBL_AUDIO_NATIVE_TRACE ready=true backend=xaudio27 behavior=observe-only");
    return 0;
}

extern "C" void __stdcall SCBL_AUDIO_SHUTDOWN() noexcept {
    if (g_router != nullptr) {
        // Shutdown is invoked from the host DLL's process-detach path while
        // the Windows loader lock is held. Signal only: waiting for, deleting,
        // or unloading the worker here can deadlock against DLL thread detach.
        g_router->request_stop();
        g_router = nullptr;
    }
    g_host_log.store(nullptr, std::memory_order_release);
}

#undef SCBL_AUDIO_INITIALIZE
#undef SCBL_AUDIO_SHUTDOWN

