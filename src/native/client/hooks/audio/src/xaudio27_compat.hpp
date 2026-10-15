#pragma once

#include <Windows.h>
#include <XAudio2.h>

#if defined(SCBL_AUDIO_USE_SYSTEM_XAUDIO27)

constexpr UINT32 SCBL_XAUDIO2_MAX_DEVICE_ID_LENGTH = 256;

struct XAUDIO2_DEVICE_DETAILS {
    WCHAR DeviceID[SCBL_XAUDIO2_MAX_DEVICE_ID_LENGTH];
    WCHAR DisplayName[SCBL_XAUDIO2_MAX_DEVICE_ID_LENGTH];
    UINT32 Role;
    WAVEFORMATEXTENSIBLE OutputFormat;
};

inline constexpr GUID SCBL_XAUDIO27_CLASS_ID = {
    0x5a508685, 0xa254, 0x4fba, {0x9b, 0x82, 0x9a, 0x24, 0xb0, 0x03, 0x06, 0xaf}
};
inline constexpr GUID SCBL_IXAUDIO27_INTERFACE_ID = {
    0x8bcf1f58, 0x9fe7, 0x4583, {0x8a, 0xc6, 0xe2, 0xad, 0xc4, 0x65, 0xc8, 0xbb}
};

#endif

inline HRESULT scbl_xaudio27_get_device_count(IXAudio2* engine, UINT32* count) noexcept {
#if defined(SCBL_AUDIO_USE_SYSTEM_XAUDIO27)
    if (engine == nullptr) return E_POINTER;
    using Fn = HRESULT(STDMETHODCALLTYPE*)(IXAudio2*, UINT32*);
    auto** vtable = *reinterpret_cast<void***>(engine);
    return reinterpret_cast<Fn>(vtable[3])(engine, count);
#else
    return engine != nullptr ? engine->GetDeviceCount(count) : E_POINTER;
#endif
}

inline HRESULT scbl_xaudio27_get_device_details(
    IXAudio2* engine,
    UINT32 index,
    XAUDIO2_DEVICE_DETAILS* details) noexcept {
#if defined(SCBL_AUDIO_USE_SYSTEM_XAUDIO27)
    if (engine == nullptr) return E_POINTER;
    using Fn = HRESULT(STDMETHODCALLTYPE*)(IXAudio2*, UINT32, XAUDIO2_DEVICE_DETAILS*);
    auto** vtable = *reinterpret_cast<void***>(engine);
    return reinterpret_cast<Fn>(vtable[4])(engine, index, details);
#else
    return engine != nullptr ? engine->GetDeviceDetails(index, details) : E_POINTER;
#endif
}

inline const GUID& scbl_xaudio27_class_id() noexcept {
#if defined(SCBL_AUDIO_USE_SYSTEM_XAUDIO27)
    return SCBL_XAUDIO27_CLASS_ID;
#else
    return __uuidof(XAudio2);
#endif
}

inline const GUID& scbl_xaudio27_interface_id() noexcept {
#if defined(SCBL_AUDIO_USE_SYSTEM_XAUDIO27)
    return SCBL_IXAUDIO27_INTERFACE_ID;
#else
    return __uuidof(IXAudio2);
#endif
}

