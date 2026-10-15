cmake_minimum_required(VERSION 3.24)
project(scbl_audio_builtin LANGUAGES CXX)

set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreaded$<$<CONFIG:Debug>:Debug>")

add_library(scbl_audio_builtin STATIC
    src/audio_router.cpp
    src/audio_router.hpp
    src/native_audio_session.cpp
    src/native_audio_session.hpp
    src/native_wasapi_output.cpp
    src/native_wasapi_output.hpp
    src/native_master_output.cpp
    src/native_master_output.hpp
    src/native_xaudio27_trace.cpp
    src/native_xaudio27_trace.hpp
    src/native_xaudio27_relay.cpp
    src/native_xaudio27_relay.hpp
    src/xaudio27_compat.hpp
    src/plugin.cpp
)

target_include_directories(scbl_audio_builtin PRIVATE
    "${CMAKE_CURRENT_SOURCE_DIR}/third_party/dxsdk_jun2010/Include"
)
target_compile_features(scbl_audio_builtin PRIVATE cxx_std_20)
target_compile_definitions(scbl_audio_builtin PRIVATE
    SCBL_AUDIO_BUILTIN=1
    WIN32_LEAN_AND_MEAN
    NOMINMAX
    _WIN32_WINNT=0x0A00
)
target_compile_options(scbl_audio_builtin PRIVATE /W4 /WX /permissive- /EHsc)
target_link_libraries(scbl_audio_builtin PRIVATE ole32 uuid)
