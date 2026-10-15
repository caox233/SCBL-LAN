#include "native_xaudio27_relay.hpp"
#include "native_audio_session.hpp"
#include "native_wasapi_output.hpp"

#include <XAPO.h>

#include <algorithm>
#include <atomic>
#include <cmath>
#include <cstring>
#include <cwchar>
#include <memory>
#include <new>
#include <string>
#include <vector>

namespace scbl::audio {
namespace {

constexpr UINT32 kLogWarning = 1;
constexpr UINT32 kLogInfo = 2;
constexpr UINT32 kRingFrames = 48'000 * 2;
constexpr UINT32 kMaximumChannels = 8;
constexpr GUID kTapClsid = {
    0xa938fe74, 0xe85f, 0x4e36, {0xa7, 0x63, 0x6c, 0x36, 0x4c, 0xa4, 0xcd, 0x61}
};

bool is_float_format(const WAVEFORMATEX* format) noexcept {
    // The relay owns a fixed WAVEFORMATEXTENSIBLE snapshot. Reject larger
    // descriptors before negotiation/copy, rather than retaining a truncated
    // snapshot whose cbSize would let downstream consumers read past it.
    if (format == nullptr || format->wBitsPerSample != 32
        || format->cbSize > sizeof(WAVEFORMATEXTENSIBLE) - sizeof(WAVEFORMATEX)) {
        return false;
    }
    if (format->wFormatTag == WAVE_FORMAT_IEEE_FLOAT) return true;
    if (format->wFormatTag != WAVE_FORMAT_EXTENSIBLE
        || format->cbSize < sizeof(WAVEFORMATEXTENSIBLE) - sizeof(WAVEFORMATEX)) {
        return false;
    }
    const auto* extensible = reinterpret_cast<const WAVEFORMATEXTENSIBLE*>(format);
    return InlineIsEqualGUID(extensible->SubFormat, KSDATAFORMAT_SUBTYPE_IEEE_FLOAT);
}

bool formats_match(const WAVEFORMATEX* left, const WAVEFORMATEX* right) noexcept {
    if (left == nullptr || right == nullptr) return false;
    if (left->wFormatTag != right->wFormatTag
        || left->nChannels != right->nChannels
        || left->nSamplesPerSec != right->nSamplesPerSec
        || left->nAvgBytesPerSec != right->nAvgBytesPerSec
        || left->nBlockAlign != right->nBlockAlign
        || left->wBitsPerSample != right->wBitsPerSample
        || left->cbSize != right->cbSize) {
        return false;
    }
    return left->cbSize == 0
        || std::memcmp(reinterpret_cast<const BYTE*>(left) + sizeof(WAVEFORMATEX),
            reinterpret_cast<const BYTE*>(right) + sizeof(WAVEFORMATEX), left->cbSize) == 0;
}

const XAPO_REGISTRATION_PROPERTIES& tap_registration() noexcept {
    static const XAPO_REGISTRATION_PROPERTIES properties = [] {
        XAPO_REGISTRATION_PROPERTIES value{};
        value.clsid = kTapClsid;
        wcsncpy_s(value.FriendlyName, L"SCBL native XAudio2 relay tap",
            _TRUNCATE);
        wcsncpy_s(value.CopyrightInfo, L"SCBL", _TRUNCATE);
        value.MajorVersion = 1;
        value.MinorVersion = 0;
        value.Flags = XAPO_FLAG_CHANNELS_MUST_MATCH
            | XAPO_FLAG_FRAMERATE_MUST_MATCH
            | XAPO_FLAG_BITSPERSAMPLE_MUST_MATCH
            | XAPO_FLAG_BUFFERCOUNT_MUST_MATCH
            | XAPO_FLAG_INPLACE_SUPPORTED
            | XAPO_FLAG_INPLACE_REQUIRED;
        value.MinInputBufferCount = 1;
        value.MaxInputBufferCount = 1;
        value.MinOutputBufferCount = 1;
        value.MaxOutputBufferCount = 1;
        return value;
    }();
    return properties;
}

class TapXapo final : public IXAPO {
public:
    TapXapo() noexcept
        : ring_(new (std::nothrow) std::atomic<UINT32>[static_cast<std::size_t>(kRingFrames) * kMaximumChannels]) {}

    bool usable() const noexcept { return ring_ != nullptr; }

    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID iid, void** output) override {
        if (output == nullptr) return E_POINTER;
        *output = nullptr;
        if (InlineIsEqualGUID(iid, IID_IUnknown) || InlineIsEqualGUID(iid, __uuidof(IXAPO))) {
            *output = static_cast<IXAPO*>(this);
            AddRef();
            return S_OK;
        }
        return E_NOINTERFACE;
    }
    ULONG STDMETHODCALLTYPE AddRef() override { return references_.fetch_add(1) + 1; }
    ULONG STDMETHODCALLTYPE Release() override {
        const ULONG remaining = references_.fetch_sub(1) - 1;
        if (remaining == 0) delete this;
        return remaining;
    }
    HRESULT STDMETHODCALLTYPE GetRegistrationProperties(
        XAPO_REGISTRATION_PROPERTIES** output) override {
        if (output == nullptr) return E_POINTER;
        *output = static_cast<XAPO_REGISTRATION_PROPERTIES*>(
            CoTaskMemAlloc(sizeof(XAPO_REGISTRATION_PROPERTIES)));
        if (*output == nullptr) return E_OUTOFMEMORY;
        **output = tap_registration();
        return S_OK;
    }
    HRESULT STDMETHODCALLTYPE IsInputFormatSupported(const WAVEFORMATEX* output_format,
        const WAVEFORMATEX* requested, WAVEFORMATEX** supported) override {
        if (supported != nullptr) *supported = nullptr;
        return is_float_format(requested) && formats_match(output_format, requested)
            ? S_OK : XAPO_E_FORMAT_UNSUPPORTED;
    }
    HRESULT STDMETHODCALLTYPE IsOutputFormatSupported(const WAVEFORMATEX* input_format,
        const WAVEFORMATEX* requested, WAVEFORMATEX** supported) override {
        if (supported != nullptr) *supported = nullptr;
        return is_float_format(requested) && formats_match(input_format, requested)
            ? S_OK : XAPO_E_FORMAT_UNSUPPORTED;
    }
    HRESULT STDMETHODCALLTYPE Initialize(const void*, UINT32) override { return S_OK; }
    void STDMETHODCALLTYPE Reset() override { request_discard(); }
    HRESULT STDMETHODCALLTYPE LockForProcess(UINT32 input_count,
        const XAPO_LOCKFORPROCESS_BUFFER_PARAMETERS* input, UINT32 output_count,
        const XAPO_LOCKFORPROCESS_BUFFER_PARAMETERS* output) override {
        if (input_count != 1 || output_count != 1 || input == nullptr || output == nullptr
            || !is_float_format(input[0].pFormat)
            || !formats_match(input[0].pFormat, output[0].pFormat)
            || input[0].pFormat->nChannels == 0
            || input[0].pFormat->nChannels > kMaximumChannels) {
            return XAPO_E_FORMAT_UNSUPPORTED;
        }
        AcquireSRWLockExclusive(&format_lock_);
        const UINT32 bytes = sizeof(WAVEFORMATEX) + input[0].pFormat->cbSize;
        std::memset(&format_, 0, sizeof(format_));
        std::memcpy(&format_, input[0].pFormat, bytes);
        channels_.store(input[0].pFormat->nChannels, std::memory_order_release);
        sample_rate_.store(input[0].pFormat->nSamplesPerSec, std::memory_order_release);
        format_bytes_.store(bytes, std::memory_order_release);
        request_discard();
        ready_.store(true, std::memory_order_release);
        format_generation_.fetch_add(1, std::memory_order_release);
        ReleaseSRWLockExclusive(&format_lock_);
        return S_OK;
    }
    void STDMETHODCALLTYPE UnlockForProcess() override {
        AcquireSRWLockExclusive(&format_lock_);
        ready_.store(false, std::memory_order_release);
        format_generation_.fetch_add(1, std::memory_order_release);
        ReleaseSRWLockExclusive(&format_lock_);
    }
    void STDMETHODCALLTYPE Process(UINT32 input_count,
        const XAPO_PROCESS_BUFFER_PARAMETERS* input, UINT32 output_count,
        XAPO_PROCESS_BUFFER_PARAMETERS* output, BOOL) override {
        if (input_count != 1 || output_count != 1 || input == nullptr || output == nullptr
            || input[0].pBuffer == nullptr || output[0].pBuffer == nullptr) {
            return;
        }
        const UINT32 channels = channels_.load(std::memory_order_relaxed);
        const UINT32 frames = input[0].ValidFrameCount;
        const std::size_t samples = static_cast<std::size_t>(frames) * channels;
        if (input[0].pBuffer != output[0].pBuffer) {
            if (input[0].BufferFlags == XAPO_BUFFER_SILENT) {
                std::memset(output[0].pBuffer, 0, samples * sizeof(float));
            } else {
                std::memcpy(output[0].pBuffer, input[0].pBuffer, samples * sizeof(float));
            }
        }
        output[0].BufferFlags = input[0].BufferFlags;
        output[0].ValidFrameCount = frames;
        push(static_cast<const float*>(input[0].pBuffer), samples,
            input[0].BufferFlags == XAPO_BUFFER_SILENT);
    }
    UINT32 STDMETHODCALLTYPE CalcInputFrames(UINT32 frames) override { return frames; }
    UINT32 STDMETHODCALLTYPE CalcOutputFrames(UINT32 frames) override { return frames; }

    // Only non-realtime lifecycle/relay-worker paths take this lock. Process
    // and Reset remain nonblocking and retain their atomic ring protocol.
    bool copy_format(WAVEFORMATEXTENSIBLE* output, UINT64* generation = nullptr) const noexcept {
        if (output == nullptr) return false;
        AcquireSRWLockShared(&format_lock_);
        const UINT32 bytes = format_bytes_.load(std::memory_order_acquire);
        const bool valid = ready_.load(std::memory_order_acquire)
            && bytes >= sizeof(WAVEFORMATEX) && bytes <= sizeof(format_);
        if (valid) {
            std::memcpy(output, &format_, bytes);
            if (generation != nullptr) *generation = format_generation_.load(std::memory_order_relaxed);
        }
        ReleaseSRWLockShared(&format_lock_);
        return valid;
    }
    UINT64 format_generation() const noexcept {
        return format_generation_.load(std::memory_order_acquire);
    }
    void read_format(float* output, std::size_t count, UINT64 generation) noexcept {
        if (output == nullptr) return;
        AcquireSRWLockShared(&format_lock_);
        if (ready_.load(std::memory_order_acquire)
            && format_generation_.load(std::memory_order_relaxed) == generation) {
            read(output, count);
        } else {
            // The old WASAPI buffer must never interpret a new format's PCM.
            std::memset(output, 0, count * sizeof(float));
        }
        ReleaseSRWLockShared(&format_lock_);
    }
    UINT32 channels() const noexcept { return channels_.load(std::memory_order_acquire); }
    UINT32 sample_rate() const noexcept { return sample_rate_.load(std::memory_order_acquire); }
    // Relay-worker-only, serialized with read(). XAPO callbacks publish their
    // independent cutoff instead of competing to write the reader cursor.
    void discard() noexcept {
        read_samples_.store(std::max(read_samples_.load(std::memory_order_relaxed),
            write_samples_.load(std::memory_order_acquire)), std::memory_order_release);
    }
    void read(float* output, std::size_t requested_samples) noexcept {
        if (output == nullptr || requested_samples == 0 || ring_ == nullptr) return;
        const std::size_t capacity = static_cast<std::size_t>(kRingFrames) * kMaximumChannels;
        const UINT64 write = write_samples_.load(std::memory_order_acquire);
        UINT64 read = read_samples_.load(std::memory_order_relaxed);
        read = std::max(read, reset_samples_.load(std::memory_order_acquire));
        // Reset can publish a cutoff after our completed-write snapshot. Do not
        // subtract an inconsistent pair, and do not clear its persistent floor.
        if (read >= write) {
            std::memset(output, 0, requested_samples * sizeof(float));
            read_samples_.store(read, std::memory_order_release);
            return;
        }
        if (write - read > capacity) {
            read = write - capacity;
            // Retain whole channel frames even when capacity is not divisible
            // by the locked channel count (for example, seven channels).
            const UINT32 channels = channels_.load(std::memory_order_relaxed);
            if (channels != 0 && read % channels != 0) read += channels - read % channels;
        }
        const std::size_t available = static_cast<std::size_t>(std::min<UINT64>(write - read, requested_samples));
        std::size_t index = static_cast<std::size_t>(read % capacity);
        for (std::size_t i = 0; i < available; ++i) {
            const UINT32 bits = ring_[index].load(std::memory_order_acquire);
            std::memcpy(output + i, &bits, sizeof(bits));
            if (++index == capacity) index = 0;
        }
        // An overwritten cell's acquire load also observes the producer's
        // preceding announcement. Reject the entire stale snapshot, including
        // partial channel frames; never retry or wait for the callback.
        if (available != 0 && announced_samples_.load(std::memory_order_acquire) - read > capacity) {
            std::memset(output, 0, requested_samples * sizeof(float));
        } else if (available < requested_samples) {
            std::memset(output + available, 0, (requested_samples - available) * sizeof(float));
        }
        read_samples_.store(read + available, std::memory_order_release);
        // A Reset during copying or cursor commit cannot be undone by that
        // commit: the independent cutoff persists, and this output is stale.
        if (reset_samples_.load(std::memory_order_acquire) > read) {
            std::memset(output, 0, requested_samples * sizeof(float));
        }
    }

private:
    // Reset and Process are engine-serialized on the realtime thread while
    // locked; LockForProcess establishes this cutoff before that phase. Only
    // these XAPO lifecycle callbacks write it, so no multiwriter max/retry is needed.
    void request_discard() noexcept {
        reset_samples_.store(write_samples_.load(std::memory_order_acquire), std::memory_order_release);
    }
    void push(const float* input, std::size_t samples, bool silent) noexcept {
        if (ring_ == nullptr || samples == 0) return;
        const std::size_t capacity = static_cast<std::size_t>(kRingFrames) * kMaximumChannels;
        const UINT64 write = write_samples_.load(std::memory_order_relaxed);
        const UINT64 end = write + samples;
        // A declared processing block may exceed this bounded history buffer.
        // Retain only its newest samples without touching the passthrough data
        // or allocating on the real-time processing thread.
        if (samples > capacity) {
            if (!silent) input += samples - capacity;
            samples = capacity;
        }
        // Publish overwrite intent before touching any atomic payload cell.
        // Release payload stores carry this announcement to an acquiring reader
        // even while the completed-write cursor still names the previous block.
        announced_samples_.store(end, std::memory_order_release);
        std::size_t index = static_cast<std::size_t>((end - samples) % capacity);
        for (std::size_t i = 0; i < samples; ++i) {
            UINT32 bits = 0;
            if (!silent) std::memcpy(&bits, input + i, sizeof(bits));
            ring_[index].store(bits, std::memory_order_release);
            if (++index == capacity) index = 0;
        }
        write_samples_.store(end, std::memory_order_release);
    }

    std::atomic<ULONG> references_{1};
    static_assert(sizeof(UINT32) == sizeof(float));
    static_assert(std::atomic<UINT32>::is_always_lock_free);
    static_assert(std::atomic<UINT64>::is_always_lock_free);
    std::unique_ptr<std::atomic<UINT32>[]> ring_;
    mutable SRWLOCK format_lock_ = SRWLOCK_INIT;
    std::atomic<UINT64> format_generation_{0};
    WAVEFORMATEXTENSIBLE format_{};
    std::atomic<UINT32> format_bytes_{0};
    std::atomic<UINT32> channels_{0};
    std::atomic<UINT32> sample_rate_{0};
    std::atomic<bool> ready_{false};
    std::atomic<UINT64> announced_samples_{0};
    std::atomic<UINT64> write_samples_{0};
    std::atomic<UINT64> read_samples_{0};
    std::atomic<UINT64> reset_samples_{0};
};

class RelayController final {
public:
    using SetVolumeFn = HRESULT(STDMETHODCALLTYPE*)(IXAudio2Voice*, float, UINT32);
    using GetVolumeFn = void(STDMETHODCALLTYPE*)(IXAudio2Voice*, float*);

    bool attach(IXAudio2* engine, IXAudio2MasteringVoice* master,
        UINT32 device_index, NativeRelayLogFn log) noexcept {
        AcquireSRWLockExclusive(&lifecycle_lock_);
        const bool result = attach_locked(engine, master, device_index, log);
        ReleaseSRWLockExclusive(&lifecycle_lock_);
        return result;
    }

private:
    bool attach_locked(IXAudio2* engine, IXAudio2MasteringVoice* master,
        UINT32 device_index, NativeRelayLogFn log) noexcept {
        if (engine == nullptr || master == nullptr || shutdown_.load(std::memory_order_acquire)
            || attached_.load(std::memory_order_acquire)) return false;
        if (thread_ != nullptr) {
            // Normal voice creation only: never wait on the process-detach path.
            // A delayed old worker keeps ownership and may be retried later.
            constexpr DWORD kPreviousWorkerWaitMs = 1'000;
            const DWORD wait = WaitForSingleObject(thread_, kPreviousWorkerWaitMs);
            if (wait != WAIT_OBJECT_0) {
                this->log(kLogWarning, wait == WAIT_TIMEOUT
                    ? "SCBL_AUDIO_RELAY tap=false reason=previous-worker-timeout"
                    : "SCBL_AUDIO_RELAY tap=false reason=previous-worker-wait-failed");
                return false;
            }
            CloseHandle(thread_);
            thread_ = nullptr;
        }
        if (shutdown_.load(std::memory_order_acquire)) return false;
        stop_.store(false, std::memory_order_release);
        original_session_ids_.clear();
        original_endpoint_invalidated_.store(false, std::memory_order_release);
        AcquireSRWLockExclusive(&target_lock_);
        target_device_id_.clear();
        target_generation_.store(0, std::memory_order_release);
        ReleaseSRWLockExclusive(&target_lock_);
        attached_.store(true, std::memory_order_release);
        log_ = log;
        master_ = master;
        auto** vtable = *reinterpret_cast<void***>(master);
        if (vtable == nullptr) return fail_attach("master-vtable");
        set_master_volume_ = reinterpret_cast<SetVolumeFn>(vtable[12]);
        get_master_volume_ = reinterpret_cast<GetVolumeFn>(vtable[13]);
        XAUDIO2_DEVICE_DETAILS details{};
        if (FAILED(scbl_xaudio27_get_device_details(engine, device_index, &details))) {
            return fail_attach("device-details");
        }
        original_device_id_ = details.DeviceID;
        auto* tap = new (std::nothrow) TapXapo();
        if (tap == nullptr || !tap->usable()) {
            if (tap != nullptr) tap->Release();
            return fail_attach("tap-allocation");
        }
        XAUDIO2_EFFECT_DESCRIPTOR descriptor{tap, TRUE, details.OutputFormat.Format.nChannels};
        XAUDIO2_EFFECT_CHAIN chain{1, &descriptor};
        const HRESULT effect_result = master->SetEffectChain(&chain);
        if (FAILED(effect_result)) {
            tap->Release();
            return fail_attach("tap-effect-chain");
        }
        tap_ = tap;
        float volume = 1.0f;
        get_master_volume_(master, &volume);
        desired_volume_.store(volume, std::memory_order_release);
        thread_ = CreateThread(nullptr, 0, relay_thread_entry, this, 0, nullptr);
        if (thread_ == nullptr) return fail_attach("relay-thread");
        if (shutdown_.load(std::memory_order_acquire)) {
            this->log(kLogWarning, "SCBL_AUDIO_RELAY tap=false reason=shutdown");
            return false;
        }
        this->log(kLogInfo, "SCBL_AUDIO_RELAY tap=true backend=xaudio27 secondary-output=ready");
        return true;
    }

public:
    void endpoint_changed(const wchar_t* id) noexcept {
        if (id == nullptr || !attached_.load(std::memory_order_acquire)) return;
        AcquireSRWLockExclusive(&target_lock_);
        target_device_id_ = id;
        ReleaseSRWLockExclusive(&target_lock_);
        target_generation_.fetch_add(1, std::memory_order_release);
        wake_worker();
    }
    void original_endpoint_invalidated() noexcept {
        if (!original_endpoint_invalidated_.exchange(true, std::memory_order_acq_rel)) {
            log(kLogInfo, "SCBL_AUDIO_RELAY original-endpoint-invalidated=true");
        }
        wake_worker();
    }
    bool adjust_volume(IXAudio2Voice* voice, float requested, float* forwarded) noexcept {
        if (voice != master_.load(std::memory_order_acquire) || forwarded == nullptr) return false;
        desired_volume_.store(requested, std::memory_order_release);
        // Never zero the game's mastering voice. The tap is part of that
        // graph and must keep receiving real PCM while the old endpoint is
        // muted at the Windows audio-session boundary.
        *forwarded = requested;
        return true;
    }
    bool override_volume(IXAudio2Voice* voice, float* value) noexcept {
        static_cast<void>(voice);
        static_cast<void>(value);
        return false;
    }
    void master_destroying(IXAudio2Voice* voice) noexcept {
        AcquireSRWLockExclusive(&lifecycle_lock_);
        if (voice != nullptr && voice == master_.load(std::memory_order_acquire)) {
            stop_.store(true, std::memory_order_release);
            master_.store(nullptr, std::memory_order_release);
            attached_.store(false, std::memory_order_release);
            wake_worker();
        }
        ReleaseSRWLockExclusive(&lifecycle_lock_);
    }
    void request_stop() noexcept {
        shutdown_.store(true, std::memory_order_release);
        stop_.store(true, std::memory_order_release);
        wake_worker();
    }

private:
    void wake_worker() const noexcept {
        const HANDLE wake = wake_.load(std::memory_order_acquire);
        if (wake != nullptr) SetEvent(wake);
    }
    static DWORD WINAPI relay_thread_entry(void* context) noexcept {
        static_cast<RelayController*>(context)->run();
        return 0;
    }
    bool fail_attach(const char* reason) noexcept {
        char message[128]{};
        std::snprintf(message, sizeof(message), "SCBL_AUDIO_RELAY tap=false reason=%s", reason);
        log(kLogWarning, message);
        // SetEffectChain owns its own reference. On worker-creation failure
        // leave that inert passthrough effect to the engine; release only ours.
        if (tap_ != nullptr) { tap_->Release(); tap_ = nullptr; }
        master_.store(nullptr, std::memory_order_release);
        attached_.store(false, std::memory_order_release);
        return false;
    }
    void log(UINT32 level, const char* message) const noexcept {
        const NativeRelayLogFn callback = log_.load(std::memory_order_acquire);
        if (callback != nullptr) callback(level, message);
    }
    std::wstring requested_device() noexcept {
        AcquireSRWLockShared(&target_lock_);
        std::wstring result = target_device_id_;
        ReleaseSRWLockShared(&target_lock_);
        return result;
    }
    bool game_is_foreground() const noexcept {
        const HWND foreground = GetForegroundWindow();
        if (foreground == nullptr) return false;
        DWORD process_id = 0;
        GetWindowThreadProcessId(foreground, &process_id);
        return process_id == GetCurrentProcessId();
    }
    float effective_output_volume() const noexcept {
        return foreground_.load(std::memory_order_acquire)
            ? desired_volume_.load(std::memory_order_acquire) : 0.0f;
    }
    void set_primary_muted(bool muted) noexcept {
        const bool changed = set_audio_session_instances_muted(
            original_device_id_, original_session_ids_, muted);
        char message[144]{};
        std::snprintf(message, sizeof(message),
            "SCBL_AUDIO_RELAY primary-session-muted=%s applied=%s",
            muted ? "true" : "false", changed ? "true" : "false");
        log(changed ? kLogInfo : kLogWarning, message);
    }
    void set_relay_muted(bool muted) noexcept {
        if (output_ == nullptr) return;
        const bool changed = SUCCEEDED(set_wasapi_output_muted(output_, muted));
        char message[144]{};
        std::snprintf(message, sizeof(message),
            "SCBL_AUDIO_RELAY relay-session-muted=%s applied=%s",
            muted ? "true" : "false", changed ? "true" : "false");
        log(changed ? kLogInfo : kLogWarning, message);
    }
    bool start_output(const std::wstring& device_id) noexcept {
        stop_output();
        if (device_id.empty() || tap_ == nullptr) return false;
        WAVEFORMATEXTENSIBLE format{};
        if (!tap_->copy_format(&format, &output_format_generation_)) {
            log(kLogWarning, "SCBL_AUDIO_RELAY output=false reason=tap-format-not-ready");
            return false;
        }
        tap_->discard();
        HRESULT result = E_FAIL;
        output_ = start_wasapi_output(device_id.c_str(), &format.Format,
            static_cast<UINT32>(sizeof(WAVEFORMATEX) + format.Format.cbSize),
            format.Format.nChannels, fill_output, this, &result);
        if (output_ == nullptr) return output_failure("wasapi-start", result);
        set_wasapi_output_volume(output_, desired_volume_.load(std::memory_order_acquire));
        set_primary_muted(true);
        active_.store(true, std::memory_order_release);
        set_relay_muted(!foreground_.load(std::memory_order_acquire));
        log(kLogInfo, "SCBL_AUDIO_RELAY output=true backend=wasapi primary-muted=true");
        return true;
    }
    bool output_failure(const char* reason, HRESULT result) noexcept {
        char message[160]{};
        std::snprintf(message, sizeof(message),
            "SCBL_AUDIO_RELAY output=false reason=%s hr=0x%08lX",
            reason, static_cast<unsigned long>(result));
        log(kLogWarning, message);
        return false;
    }
    void stop_output() noexcept {
        set_relay_muted(false);
        if (active_.exchange(false, std::memory_order_acq_rel)) {
            set_primary_muted(!foreground_.load(std::memory_order_acquire));
        }
        stop_wasapi_output(output_);
        output_ = nullptr;
    }
    static void fill_output(void* context, float* samples, std::size_t count) noexcept {
        auto* self = static_cast<RelayController*>(context);
        if (self != nullptr && self->tap_ != nullptr) {
            self->tap_->read_format(samples, count, self->output_format_generation_);
        } else if (samples != nullptr) {
            std::memset(samples, 0, count * sizeof(float));
        }
    }
    void pump() noexcept {
        if (output_ == nullptr) return;
        const HRESULT result = pump_wasapi_output(output_);
        if (FAILED(result)) { output_failure("wasapi-pump", result); stop_output(); }
    }
    void run() noexcept {
        const HRESULT com = CoInitializeEx(nullptr, COINIT_MULTITHREADED);
        HANDLE wake = wake_.load(std::memory_order_acquire);
        if (wake == nullptr) {
            wake = CreateEventW(nullptr, FALSE, FALSE, nullptr);
            wake_.store(wake, std::memory_order_release);
        }
        foreground_.store(game_is_foreground(), std::memory_order_release);
        for (UINT32 attempt = 0; attempt < 10 && original_session_ids_.empty(); ++attempt) {
            original_session_ids_ = current_process_audio_session_instances(original_device_id_);
            if (original_session_ids_.empty()) Sleep(20);
        }
        set_primary_muted(!foreground_.load(std::memory_order_acquire));
        // A target can be queued immediately after the mastering voice is
        // created, before this worker begins running.
        UINT64 generation = 0;
        UINT64 format_generation = 0;
        UINT64 delayed_rebind_generation = 0;
        ULONGLONG delayed_rebind_due = 0;
        // Retry only failed starts for the current target/format. The worker's
        // monotonic clock and idle wait keep failures off the render callback.
        constexpr ULONGLONG kOutputRetryInitialMs = 250;
        constexpr ULONGLONG kOutputRetryMaximumMs = 2'000;
        ULONGLONG output_retry_due = 0;
        ULONGLONG output_retry_delay = kOutputRetryInitialMs;
        const auto start_or_retry = [&](const std::wstring& target) noexcept {
            output_retry_due = 0;
            if (stop_.load(std::memory_order_acquire)
                || shutdown_.load(std::memory_order_acquire)) return;
            if (start_output(target)) {
                output_retry_delay = kOutputRetryInitialMs;
            } else if (!target.empty()) {
                output_retry_due = GetTickCount64() + output_retry_delay;
                output_retry_delay = std::min(output_retry_delay * 2, kOutputRetryMaximumMs);
            }
        };
        while (!stop_.load(std::memory_order_acquire)
            && !shutdown_.load(std::memory_order_acquire)) {
            const bool current_foreground = game_is_foreground();
            const bool previous_foreground = foreground_.exchange(
                current_foreground, std::memory_order_acq_rel);
            if (current_foreground != previous_foreground) {
                set_primary_muted(active_.load(std::memory_order_acquire)
                    || !current_foreground);
                if (output_ != nullptr) {
                    set_wasapi_output_volume(
                        output_, desired_volume_.load(std::memory_order_acquire));
                }
                set_relay_muted(!current_foreground);
                log(kLogInfo, current_foreground
                    ? "SCBL_AUDIO_RELAY focus=true muted=false"
                    : "SCBL_AUDIO_RELAY focus=false muted=true");
            }
            const UINT64 current_generation = target_generation_.load(std::memory_order_acquire);
            const UINT64 current_format_generation = tap_->format_generation();
            const bool endpoint_changed = current_generation != generation;
            if (endpoint_changed || current_format_generation != format_generation) {
                output_retry_due = 0;
                output_retry_delay = kOutputRetryInitialMs;
                generation = current_generation;
                format_generation = current_format_generation;
                const std::wstring target = requested_device();
                if (target == original_device_id_
                    && !original_endpoint_invalidated_.load(std::memory_order_acquire)) {
                    // XAudio2 engines created by one process on one endpoint
                    // share a Windows audio session. A second relay on the
                    // original endpoint therefore cannot be muted separately.
                    // Restore the still-live game voice instead.
                    stop_output();
                    set_primary_muted(!foreground_.load(std::memory_order_acquire));
                    log(kLogInfo, "SCBL_AUDIO_RELAY output=native reason=original-endpoint");
                } else {
                    start_or_retry(target);
                }
                if (endpoint_changed && original_endpoint_invalidated_.load(std::memory_order_acquire)) {
                    delayed_rebind_generation = generation;
                    delayed_rebind_due = GetTickCount64() + 2'000;
                }
            }
            if (delayed_rebind_due != 0
                && delayed_rebind_generation == generation
                && GetTickCount64() >= delayed_rebind_due) {
                delayed_rebind_due = 0;
                start_or_retry(requested_device());
                log(kLogInfo,
                    "SCBL_AUDIO_RELAY endpoint-rebind=delayed reason=device-settle");
            }
            if (output_retry_due != 0 && GetTickCount64() >= output_retry_due
                && target_generation_.load(std::memory_order_acquire) == generation
                && tap_->format_generation() == format_generation) {
                start_or_retry(requested_device());
            }
            if (active_.load(std::memory_order_acquire) && output_ != nullptr) {
                set_wasapi_output_volume(
                    output_, desired_volume_.load(std::memory_order_acquire));
                pump();
            }
            const DWORD delay = active_.load(std::memory_order_acquire) ? 3 : 100;
            if (wake != nullptr) WaitForSingleObject(wake, delay);
            else Sleep(delay);
        }
        stop_output();
        set_primary_muted(false);
        // The controller and signal-only callbacks are process-lifetime.
        // Retain this single event until process exit: a callback may have
        // already loaded its handle, so atomic exchange alone cannot close it.
        // No lock or callback drain is safe in loader-lock shutdown.
        if (tap_ != nullptr) { tap_->Release(); tap_ = nullptr; }
        if (SUCCEEDED(com)) CoUninitialize();
    }

    SRWLOCK lifecycle_lock_ = SRWLOCK_INIT;
    std::atomic<bool> shutdown_{false};
    std::atomic<bool> attached_{false};
    std::atomic<bool> stop_{false};
    std::atomic<bool> active_{false};
    std::atomic<bool> foreground_{true};
    static_assert(std::atomic<NativeRelayLogFn>::is_always_lock_free);
    static_assert(std::atomic<IXAudio2MasteringVoice*>::is_always_lock_free);
    std::atomic<NativeRelayLogFn> log_{nullptr};
    std::atomic<IXAudio2MasteringVoice*> master_{nullptr};
    SetVolumeFn set_master_volume_ = nullptr;
    GetVolumeFn get_master_volume_ = nullptr;
    TapXapo* tap_ = nullptr;
    std::wstring original_device_id_;
    std::vector<std::wstring> original_session_ids_;
    std::atomic<bool> original_endpoint_invalidated_{false};
    std::atomic<float> desired_volume_{1.0f};
    SRWLOCK target_lock_ = SRWLOCK_INIT;
    std::wstring target_device_id_;
    std::atomic<UINT64> target_generation_{0};
    HANDLE thread_ = nullptr;
    std::atomic<HANDLE> wake_{nullptr};
    static_assert(std::atomic<HANDLE>::is_always_lock_free);
    UINT64 output_format_generation_ = 0;
    WasapiOutput* output_ = nullptr;
};

RelayController* relay() noexcept {
    static RelayController* instance = new (std::nothrow) RelayController();
    return instance;
}

} // namespace

bool attach_native_xaudio27_relay(IXAudio2* engine, IXAudio2MasteringVoice* master,
    UINT32 device_index, NativeRelayLogFn log) noexcept {
    RelayController* controller = relay();
    return controller != nullptr && controller->attach(engine, master, device_index, log);
}
void native_xaudio27_relay_endpoint_changed(const wchar_t* endpoint_id) noexcept {
    if (RelayController* controller = relay(); controller != nullptr) controller->endpoint_changed(endpoint_id);
}
void native_xaudio27_relay_original_endpoint_invalidated() noexcept {
    if (RelayController* controller = relay(); controller != nullptr) {
        controller->original_endpoint_invalidated();
    }
}
bool native_xaudio27_relay_adjust_master_volume(IXAudio2Voice* voice, float requested,
    float* forwarded) noexcept {
    RelayController* controller = relay();
    return controller != nullptr && controller->adjust_volume(voice, requested, forwarded);
}
bool native_xaudio27_relay_override_master_volume(IXAudio2Voice* voice, float* value) noexcept {
    RelayController* controller = relay();
    return controller != nullptr && controller->override_volume(voice, value);
}
void native_xaudio27_relay_master_destroying(IXAudio2Voice* voice) noexcept {
    if (RelayController* controller = relay(); controller != nullptr) controller->master_destroying(voice);
}
void native_xaudio27_relay_request_stop() noexcept {
    if (RelayController* controller = relay(); controller != nullptr) controller->request_stop();
}

} // namespace scbl::audio

