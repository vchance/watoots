# The second C++ host application: the asset world, with the host owning the
# codec. A separate binary rather than a mode on `host_cpp`, because the two
# demonstrate different things and a host that switched worlds on argv would
# demonstrate neither clearly.
include(FetchContent)

# stb, for PNG decode and encode. Fetched and pinned exactly as googletest is
# (crates/host-capi/tests/CMakeLists.txt), so nothing third-party lands in the
# tree and the version is a commit rather than "whatever was on master".
#
# stb ships headers and no build system, so there is nothing to configure:
# SOURCE_SUBDIR names a directory that does not exist, which is how CMake
# documents "populate this but do not call add_subdirectory()". SYSTEM is what
# keeps the headers out of -Wall -Werror *and* out of clang-tidy -- both honour
# -isystem, so third-party code is not linted while `main.cc` still is.
FetchContent_Declare(
  stb
  GIT_REPOSITORY https://github.com/nothings/stb.git
  GIT_TAG 2c980bb59875b0d32144a71867fbdebb2f77cd20 # master, 2026-08-01
  GIT_SHALLOW FALSE
  SOURCE_SUBDIR no-cmake-build-here
  SYSTEM
  EXCLUDE_FROM_ALL)
FetchContent_MakeAvailable(stb)

add_library(watoots_stb INTERFACE)
target_include_directories(watoots_stb SYSTEM INTERFACE ${stb_SOURCE_DIR})

# stb_impl.cc is nothing but the two implementation macros and their includes,
# so every line clang-tidy could complain about is inside a -isystem header and
# is suppressed there rather than by a NOLINT here.
add_executable(host_cpp_asset main.cc stb_impl.cc)
target_link_libraries(host_cpp_asset PRIVATE watoots::capi watoots_stb)
target_compile_features(host_cpp_asset PRIVATE cxx_std_20)
target_compile_options(host_cpp_asset PRIVATE ${WATOOTS_STRICT_FLAGS})

# Run the host against whichever asset guests have been built, exactly as
# examples/host-cpp does: each needs a different toolchain, so a missing one is
# skipped rather than failed.
#
# The pipeline resizes first on purpose. `resize` costs one pass per
# *destination* pixel and every later step costs one pass per pixel it is
# handed, so shrinking up front keeps the interpreted guests -- a CPython and a
# SpiderMonkey, both running under an unoptimised wasmtime in the `dev` preset
# -- inside the fuel and timeout their shipped policies grant.
#
# The `lut` path is absolute because the component path is: a directory is
# preopened under the same string the grant expanded to, and `${plugin_dir}`
# here expands from an absolute path. See `operation.lut` in asset.wit.
foreach(sample rust cpp js py)
  string(REPLACE "-" "_" sample_file "${sample}_asset")
  set(plugin_dir ${PROJECT_SOURCE_DIR}/examples/plugins/${sample}-asset)
  set(component ${plugin_dir}/${sample_file}.wasm)
  if(EXISTS ${component})
    add_test(
      NAME host_cpp_asset.${sample}
      COMMAND
        host_cpp_asset ${component}
        ${PROJECT_SOURCE_DIR}/examples/policies/${sample}-asset.toml
        ${CMAKE_CURRENT_SOURCE_DIR}/input.png
        ${CMAKE_CURRENT_BINARY_DIR}/${sample}-out.png
        resize:32x32,grayscale,lut:${plugin_dir}/luts/sepia.lut)
    # Four guests, one expected line: the world is the whole contract, and the
    # bytes are part of it -- `crates/host/tests/asset_e2e.rs` is where that is
    # asserted value by value.
    set_tests_properties(
      host_cpp_asset.${sample} PROPERTIES PASS_REGULAR_EXPRESSION
                                          "wrote .*32x32 RGB8")
  endif()
endforeach()
