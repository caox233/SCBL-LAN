#pragma once

#include <Windows.h>

namespace scbl::audio {

using LogFn = void(*)(UINT32 level, const char* message) noexcept;

class Router final {
public:
    Router() noexcept;
    ~Router();

    Router(const Router&) = delete;
    Router& operator=(const Router&) = delete;

    bool start(LogFn log) noexcept;
    void request_stop() noexcept;
    void stop() noexcept;

private:
    struct State;
    State* state_;
};

} // namespace scbl::audio

