#include "native_master_output.hpp"

namespace scbl::audio {

MasterOutputResult create_master_output(IXAudio2* self, IXAudio2MasteringVoice** output,
    const MasterOutputRequest& request, CreateMasterVoiceFn create, NativeRelayLogFn log) noexcept {
    const auto [channels, rate, flags, device, effects] = request;
    UINT32 anchor_device = device;
    XAUDIO2_DEVICE_DETAILS requested_details{};
    UINT32 device_count = 0;
    const bool requested_known = SUCCEEDED(
        scbl_xaudio27_get_device_details(self, device, &requested_details));
    // Do not redirect unless the requested endpoint can be restored by the relay.
    if (requested_known && requested_details.DeviceID[0] != L'\0'
        && (effects == nullptr || effects->EffectCount == 0)
        && SUCCEEDED(scbl_xaudio27_get_device_count(self, &device_count)) && device_count > 1) {
        for (UINT32 candidate = 0; candidate < device_count; ++candidate) {
            if (candidate != device) {
                XAUDIO2_DEVICE_DETAILS candidate_details{};
                if (SUCCEEDED(scbl_xaudio27_get_device_details(
                        self, candidate, &candidate_details))) {
                    anchor_device = candidate;
                    break;
                }
            }
        }
    }
    HRESULT result = create(self, output, channels, rate, flags, anchor_device, effects);
    if (FAILED(result) && anchor_device != device) {
        anchor_device = device;
        result = create(self, output, channels, rate, flags, device, effects);
    }
    if (SUCCEEDED(result) && output != nullptr && *output != nullptr) {
        if (effects == nullptr || effects->EffectCount == 0) {
            const bool attached = attach_native_xaudio27_relay(
                self, *output, anchor_device, log);
            if (attached && anchor_device != device && requested_known) {
                native_xaudio27_relay_endpoint_changed(requested_details.DeviceID);
                if (log != nullptr) log(2,
                    "SCBL_AUDIO_RELAY anchor=true initial-output=default-wasapi");
            } else if (!attached && anchor_device != device) {
                // The alternate voice has not been returned or traced yet.
                // A failed relay must not change the game's requested output.
                native_xaudio27_relay_master_destroying(*output);
                (*output)->DestroyVoice();
                *output = nullptr;
                anchor_device = device;
                result = create(self, output, channels, rate, flags, device, effects);
                if (log != nullptr) log(1,
                    "SCBL_AUDIO_RELAY tap=false fallback=requested-device");
            }
        } else {
            if (log != nullptr) log(1,
                "SCBL_AUDIO_RELAY tap=false reason=existing-master-effect-chain");
        }
    }
    return {result, anchor_device};
}

} // namespace scbl::audio
