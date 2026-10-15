#pragma once

#include <Windows.h>

namespace scbl::audio {

using NativeTraceLogFn = void(*)(UINT32 level, const char* message) noexcept;

bool install_native_xaudio27_trace(NativeTraceLogFn log) noexcept;
void request_native_audio_recovery_stop() noexcept;

} // namespace scbl::audio

