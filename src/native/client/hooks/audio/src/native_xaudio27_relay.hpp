#pragma once

#include <Windows.h>
#include "xaudio27_compat.hpp"

namespace scbl::audio {

using NativeRelayLogFn = void(*)(UINT32 level, const char* message) noexcept;

bool attach_native_xaudio27_relay(
    IXAudio2* engine,
    IXAudio2MasteringVoice* master,
    UINT32 device_index,
    NativeRelayLogFn log) noexcept;

void native_xaudio27_relay_endpoint_changed(const wchar_t* endpoint_id) noexcept;
void native_xaudio27_relay_original_endpoint_invalidated() noexcept;
bool native_xaudio27_relay_adjust_master_volume(
    IXAudio2Voice* voice,
    float requested,
    float* forwarded) noexcept;
bool native_xaudio27_relay_override_master_volume(
    IXAudio2Voice* voice,
    float* value) noexcept;
void native_xaudio27_relay_master_destroying(IXAudio2Voice* voice) noexcept;
void native_xaudio27_relay_request_stop() noexcept;

} // namespace scbl::audio

