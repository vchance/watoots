# C/C++ side of watoots. The Rust workspace is built by cargo; this tree builds
# the shipped headers, their tests, and (from M3) the C API library and the
# CMake package that C++ hosts consume. See docs/adr/0003-cpp-toolchain.md.
cmake_minimum_required(VERSION 3.28)

project(
  watoots
  VERSION 0.0.0
  DESCRIPTION "Sandboxed plugin host for the WebAssembly component model"
  LANGUAGES C CXX)

# C++20 is the floor for the shipped headers (ADR-0003); consumers already on
# C++23 pick up std::expected automatically. Extensions off so -Wpedantic means
# something and so we notice accidental GNU-isms in a public header.
set(CMAKE_C_EXTENSIONS OFF)
set(CMAKE_CXX_EXTENSIONS OFF)
set(CMAKE_EXPORT_COMPILE_COMMANDS ON)

# We ship headers, not C++20 modules. Leaving module scanning on makes CMake put
# @...modmap arguments into compile_commands.json that only exist after a build,
# which breaks clang-tidy on a freshly configured tree.
set(CMAKE_CXX_SCAN_FOR_MODULES OFF)

# Match the deployment target rustc uses, or every link of the Rust staticlib
# warns once per object file that it was built for a newer macOS than we are
# linking for. rustc will tell us, so we do not have to guess or hardcode.
if(APPLE AND NOT CMAKE_OSX_DEPLOYMENT_TARGET)
  execute_process(
    COMMAND rustc --print deployment-target
    OUTPUT_VARIABLE watoots_rustc_deployment
    OUTPUT_STRIP_TRAILING_WHITESPACE
    ERROR_QUIET)
  if(watoots_rustc_deployment MATCHES "=([0-9]+\\.[0-9]+)")
    set(CMAKE_OSX_DEPLOYMENT_TARGET
        ${CMAKE_MATCH_1}
        CACHE STRING "macOS deployment target, matched to rustc" FORCE)
    message(STATUS "watoots: macOS deployment target ${CMAKE_MATCH_1} (from rustc)")
  endif()
endif()

option(WATOOTS_BUILD_TESTS "Build the C/C++ test suite" ${PROJECT_IS_TOP_LEVEL})
option(WATOOTS_BUILD_CAPI "Build the Rust C API library with cargo" ON)
option(WATOOTS_ENABLE_SANITIZERS "Build with AddressSanitizer and UBSan" OFF)

if(MSVC)
  set(WATOOTS_STRICT_FLAGS /W4 /WX /permissive-)
else()
  set(WATOOTS_STRICT_FLAGS -Wall -Wextra -Wpedantic -Werror)
endif()

if(WATOOTS_ENABLE_SANITIZERS)
  if(MSVC)
    message(FATAL_ERROR "WATOOTS_ENABLE_SANITIZERS requires clang or gcc")
  endif()
  # LeakSanitizer is Linux-only, so leaks are caught in CI rather than locally
  # on macOS. ASan and UBSan run everywhere.
  add_compile_options(-fsanitize=address,undefined -fno-omit-frame-pointer -g)
  add_link_options(-fsanitize=address,undefined)
endif()

add_subdirectory(crates/host-capi)

if(WATOOTS_BUILD_CAPI)
  add_subdirectory(examples/host-cpp)
endif()

if(WATOOTS_BUILD_TESTS)
  enable_testing()
  add_subdirectory(crates/host-capi/tests)
endif()
