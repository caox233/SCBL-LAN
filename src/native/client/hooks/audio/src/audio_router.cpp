#include "audio_router.hpp"
#include "native_xaudio27_trace.hpp"

#include <new>

namespace scbl::audio {

struct Router::State {};

Router::Router() noexcept : state_(nullptr) {}
Router::~Router() { stop(); }

bool Router::start(LogFn log) noexcept {
    if (state_ != nullptr) return true;
    if (!install_native_xaudio27_trace(log)) return false;
    state_ = new (std::nothrow) State();
    return state_ != nullptr;
}

void Router::request_stop() noexcept {
    // The trace is process-lifetime. The host calls plugin shutdown under the
    // loader lock, so no unpatching or waiting is permitted here.
    request_native_audio_recovery_stop();
}

void Router::stop() noexcept {
    state_ = nullptr;
}

} // namespace scbl::audio

