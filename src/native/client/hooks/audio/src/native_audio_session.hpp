#pragma once

#include <string>
#include <vector>

namespace scbl::audio {

std::vector<std::wstring> current_process_audio_session_instances(
    const std::wstring& endpoint_id) noexcept;

bool set_audio_session_instances_muted(
    const std::wstring& endpoint_id,
    const std::vector<std::wstring>& instance_ids,
    bool muted) noexcept;

} // namespace scbl::audio

