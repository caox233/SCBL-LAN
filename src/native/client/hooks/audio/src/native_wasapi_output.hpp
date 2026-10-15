#pragma once

#include <Windows.h>

#include <cstddef>

namespace scbl::audio {

using WasapiFillFn = void(*)(void* context, float* samples, std::size_t count) noexcept;

struct WasapiOutput;

WasapiOutput* start_wasapi_output(
    const wchar_t* endpoint_id,
    const void* wave_format,
    UINT32 wave_format_bytes,
    UINT32 channels,
    WasapiFillFn fill,
    void* fill_context,
    HRESULT* result) noexcept;

HRESULT pump_wasapi_output(WasapiOutput* output) noexcept;
HRESULT set_wasapi_output_volume(WasapiOutput* output, float volume) noexcept;
HRESULT set_wasapi_output_muted(WasapiOutput* output, bool muted) noexcept;
void stop_wasapi_output(WasapiOutput* output) noexcept;

} // namespace scbl::audio

