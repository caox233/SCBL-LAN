#pragma once

#include "native_xaudio27_relay.hpp"

namespace scbl::audio {

struct MasterOutputRequest {
    UINT32 channels;
    UINT32 rate;
    UINT32 flags;
    UINT32 device;
    const XAUDIO2_EFFECT_CHAIN* effects;
};

struct MasterOutputResult {
    HRESULT status;
    UINT32 device;
};

using CreateMasterVoiceFn = HRESULT(STDMETHODCALLTYPE*)(IXAudio2*, IXAudio2MasteringVoice**,
    UINT32, UINT32, UINT32, UINT32, const XAUDIO2_EFFECT_CHAIN*);

// Returns only the final native voice/result. Any rejected alternate voice is
// detached and destroyed before retrying the requested output. The caller owns
// successful voices and tracing; the relay owns its existing worker lifetime.
MasterOutputResult create_master_output(IXAudio2* engine, IXAudio2MasteringVoice** output,
    const MasterOutputRequest& request, CreateMasterVoiceFn create, NativeRelayLogFn log) noexcept;

} // namespace scbl::audio
