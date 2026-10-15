#include "native_wasapi_output.hpp"

#include <audioclient.h>
#include <audiopolicy.h>
#include <mmdeviceapi.h>

#include <new>

namespace scbl::audio {
namespace {

constexpr GUID kRelaySessionGuid = {
    0xb9c535b5, 0x37c6, 0x43dd, {0x93, 0x36, 0x0e, 0x54, 0x41, 0xd9, 0x5f, 0x8e}
};

template <typename T>
void release(T*& value) noexcept {
    if (value != nullptr) {
        value->Release();
        value = nullptr;
    }
}

} // namespace

struct WasapiOutput {
    IMMDevice* device = nullptr;
    IAudioClient* client = nullptr;
    IAudioRenderClient* render = nullptr;
    ISimpleAudioVolume* volume = nullptr;
    UINT32 buffer_frames = 0;
    UINT32 channels = 0;
    WasapiFillFn fill = nullptr;
    void* fill_context = nullptr;
};

HRESULT pump_wasapi_output(WasapiOutput* output) noexcept {
    if (output == nullptr || output->client == nullptr || output->render == nullptr
        || output->fill == nullptr || output->channels == 0) {
        return E_POINTER;
    }
    UINT32 padding = 0;
    HRESULT result = output->client->GetCurrentPadding(&padding);
    if (FAILED(result) || padding >= output->buffer_frames) return result;
    const UINT32 frames = output->buffer_frames - padding;
    BYTE* bytes = nullptr;
    result = output->render->GetBuffer(frames, &bytes);
    if (FAILED(result)) return result;
    output->fill(output->fill_context, reinterpret_cast<float*>(bytes),
        static_cast<std::size_t>(frames) * output->channels);
    return output->render->ReleaseBuffer(frames, 0);
}

WasapiOutput* start_wasapi_output(const wchar_t* endpoint_id, const void* wave_format,
    UINT32 wave_format_bytes, UINT32 channels, WasapiFillFn fill, void* fill_context,
    HRESULT* result) noexcept {
    if (result != nullptr) *result = E_FAIL;
    if (endpoint_id == nullptr || wave_format == nullptr
        || wave_format_bytes < sizeof(WAVEFORMATEX) || channels == 0 || fill == nullptr) {
        if (result != nullptr) *result = E_INVALIDARG;
        return nullptr;
    }
    auto* output = new (std::nothrow) WasapiOutput();
    if (output == nullptr) {
        if (result != nullptr) *result = E_OUTOFMEMORY;
        return nullptr;
    }
    output->channels = channels;
    output->fill = fill;
    output->fill_context = fill_context;

    IMMDeviceEnumerator* enumerator = nullptr;
    HRESULT current = CoCreateInstance(__uuidof(MMDeviceEnumerator), nullptr,
        CLSCTX_INPROC_SERVER, IID_PPV_ARGS(&enumerator));
    if (SUCCEEDED(current)) current = enumerator->GetDevice(endpoint_id, &output->device);
    release(enumerator);
    if (SUCCEEDED(current)) {
        current = output->device->Activate(__uuidof(IAudioClient), CLSCTX_INPROC_SERVER,
            nullptr, reinterpret_cast<void**>(&output->client));
    }
    constexpr DWORD flags = AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM
        | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
    if (SUCCEEDED(current)) {
        current = output->client->Initialize(AUDCLNT_SHAREMODE_SHARED, flags,
            1'000'000, 0, static_cast<const WAVEFORMATEX*>(wave_format), &kRelaySessionGuid);
    }
    if (SUCCEEDED(current)) current = output->client->GetBufferSize(&output->buffer_frames);
    if (SUCCEEDED(current)) {
        current = output->client->GetService(
            __uuidof(IAudioRenderClient), reinterpret_cast<void**>(&output->render));
    }
    if (SUCCEEDED(current)) {
        current = output->client->GetService(
            __uuidof(ISimpleAudioVolume), reinterpret_cast<void**>(&output->volume));
    }
    if (SUCCEEDED(current)) current = pump_wasapi_output(output);
    if (SUCCEEDED(current)) current = output->client->Start();
    if (FAILED(current)) {
        stop_wasapi_output(output);
        if (result != nullptr) *result = current;
        return nullptr;
    }
    if (result != nullptr) *result = S_OK;
    return output;
}

HRESULT set_wasapi_output_volume(WasapiOutput* output, float volume) noexcept {
    return output != nullptr && output->volume != nullptr
        ? output->volume->SetMasterVolume(volume, nullptr) : E_POINTER;
}

HRESULT set_wasapi_output_muted(WasapiOutput* output, bool muted) noexcept {
    return output != nullptr && output->volume != nullptr
        ? output->volume->SetMute(muted, nullptr) : E_POINTER;
}

void stop_wasapi_output(WasapiOutput* output) noexcept {
    if (output == nullptr) return;
    if (output->volume != nullptr) output->volume->SetMute(FALSE, nullptr);
    if (output->client != nullptr) output->client->Stop();
    release(output->volume);
    release(output->render);
    release(output->client);
    release(output->device);
    delete output;
}

} // namespace scbl::audio

