#include "native_audio_session.hpp"

#include <Windows.h>
#include <audiopolicy.h>
#include <mmdeviceapi.h>

#include <algorithm>

namespace scbl::audio {
namespace {

template<typename T>
void release_interface(T*& value) noexcept {
    if (value != nullptr) {
        value->Release();
        value = nullptr;
    }
}

} // namespace

std::vector<std::wstring> current_process_audio_session_instances(
    const std::wstring& endpoint_id) noexcept {
    std::vector<std::wstring> result_ids;
    if (endpoint_id.empty()) return result_ids;
    IMMDeviceEnumerator* enumerator = nullptr;
    IMMDevice* device = nullptr;
    IAudioSessionManager2* manager = nullptr;
    IAudioSessionEnumerator* sessions = nullptr;
    HRESULT result = CoCreateInstance(__uuidof(MMDeviceEnumerator), nullptr, CLSCTX_INPROC_SERVER,
        __uuidof(IMMDeviceEnumerator), reinterpret_cast<void**>(&enumerator));
    if (SUCCEEDED(result)) result = enumerator->GetDevice(endpoint_id.c_str(), &device);
    if (SUCCEEDED(result)) {
        result = device->Activate(__uuidof(IAudioSessionManager2), CLSCTX_INPROC_SERVER,
            nullptr, reinterpret_cast<void**>(&manager));
    }
    if (SUCCEEDED(result)) result = manager->GetSessionEnumerator(&sessions);
    int count = 0;
    if (SUCCEEDED(result)) result = sessions->GetCount(&count);
    for (int index = 0; SUCCEEDED(result) && index < count; ++index) {
        IAudioSessionControl* control = nullptr;
        IAudioSessionControl2* control2 = nullptr;
        DWORD process_id = 0;
        LPWSTR instance_id = nullptr;
        if (SUCCEEDED(sessions->GetSession(index, &control))
            && SUCCEEDED(control->QueryInterface(__uuidof(IAudioSessionControl2),
                reinterpret_cast<void**>(&control2)))
            && SUCCEEDED(control2->GetProcessId(&process_id))
            && process_id == GetCurrentProcessId()
            && SUCCEEDED(control2->GetSessionInstanceIdentifier(&instance_id))
            && instance_id != nullptr) {
            result_ids.emplace_back(instance_id);
        }
        if (instance_id != nullptr) CoTaskMemFree(instance_id);
        release_interface(control2);
        release_interface(control);
    }
    release_interface(sessions);
    release_interface(manager);
    release_interface(device);
    release_interface(enumerator);
    return result_ids;
}

bool set_audio_session_instances_muted(
    const std::wstring& endpoint_id,
    const std::vector<std::wstring>& instance_ids,
    bool muted) noexcept {
    if (endpoint_id.empty() || instance_ids.empty()) return false;
    IMMDeviceEnumerator* enumerator = nullptr;
    IMMDevice* device = nullptr;
    IAudioSessionManager2* manager = nullptr;
    IAudioSessionEnumerator* sessions = nullptr;
    HRESULT result = CoCreateInstance(__uuidof(MMDeviceEnumerator), nullptr, CLSCTX_INPROC_SERVER,
        __uuidof(IMMDeviceEnumerator), reinterpret_cast<void**>(&enumerator));
    if (SUCCEEDED(result)) result = enumerator->GetDevice(endpoint_id.c_str(), &device);
    if (SUCCEEDED(result)) {
        result = device->Activate(__uuidof(IAudioSessionManager2), CLSCTX_INPROC_SERVER,
            nullptr, reinterpret_cast<void**>(&manager));
    }
    if (SUCCEEDED(result)) result = manager->GetSessionEnumerator(&sessions);
    int count = 0;
    if (SUCCEEDED(result)) result = sessions->GetCount(&count);
    bool changed = false;
    for (int index = 0; SUCCEEDED(result) && index < count; ++index) {
        IAudioSessionControl* control = nullptr;
        IAudioSessionControl2* control2 = nullptr;
        ISimpleAudioVolume* volume = nullptr;
        DWORD process_id = 0;
        LPWSTR instance_id = nullptr;
        if (SUCCEEDED(sessions->GetSession(index, &control))
            && SUCCEEDED(control->QueryInterface(__uuidof(IAudioSessionControl2),
                reinterpret_cast<void**>(&control2)))
            && SUCCEEDED(control2->GetProcessId(&process_id))
            && process_id == GetCurrentProcessId()
            && SUCCEEDED(control2->GetSessionInstanceIdentifier(&instance_id))
            && instance_id != nullptr
            && std::find(instance_ids.begin(), instance_ids.end(), instance_id) != instance_ids.end()
            && SUCCEEDED(control->QueryInterface(__uuidof(ISimpleAudioVolume),
                reinterpret_cast<void**>(&volume)))
            && SUCCEEDED(volume->SetMute(muted ? TRUE : FALSE, nullptr))) {
            changed = true;
        }
        if (instance_id != nullptr) CoTaskMemFree(instance_id);
        release_interface(volume);
        release_interface(control2);
        release_interface(control);
    }
    release_interface(sessions);
    release_interface(manager);
    release_interface(device);
    release_interface(enumerator);
    return changed;
}

} // namespace scbl::audio

