# Header-only for now. M3 adds the cbindgen-generated header, the Rust
# staticlib, and the install/export rules that make watoots::capi a findable
# CMake package. How cargo gets driven from CMake is still an open decision
# (see docs/SPEC.md); nothing here commits to an answer.

add_library(watoots_capi INTERFACE)
add_library(watoots::capi ALIAS watoots_capi)

target_include_directories(
  watoots_capi INTERFACE $<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/include>)

target_sources(
  watoots_capi
  INTERFACE FILE_SET
            HEADERS
            BASE_DIRS
            ${CMAKE_CURRENT_SOURCE_DIR}/include
            FILES
            ${CMAKE_CURRENT_SOURCE_DIR}/include/watoots.h
            ${CMAKE_CURRENT_SOURCE_DIR}/include/watoots.hpp)

# The floor, imposed on every consumer.
target_compile_features(watoots_capi INTERFACE c_std_11 cxx_std_20)
