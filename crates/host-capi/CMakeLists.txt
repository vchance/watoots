# The C API: a Rust staticlib plus the two shipped headers.
#
# Cargo runs only when *we* build. A consumer of the installed package links a
# prebuilt library and reads committed headers, and needs no Rust toolchain.
# See docs/adr/0005-cargo-from-cmake.md.

add_library(watoots_capi INTERFACE)
add_library(watoots::capi ALIAS watoots_capi)

target_include_directories(
  watoots_capi INTERFACE $<BUILD_INTERFACE:${CMAKE_CURRENT_SOURCE_DIR}/include>
                         $<INSTALL_INTERFACE:include>)

target_sources(
  watoots_capi
  INTERFACE FILE_SET
            HEADERS
            BASE_DIRS
            ${CMAKE_CURRENT_SOURCE_DIR}/include
            FILES
            ${CMAKE_CURRENT_SOURCE_DIR}/include/watoots.h
            ${CMAKE_CURRENT_SOURCE_DIR}/include/watoots.hpp)

# The floor, imposed on every consumer (ADR-0003).
target_compile_features(watoots_capi INTERFACE c_std_11 cxx_std_20)

if(NOT WATOOTS_BUILD_CAPI)
  message(STATUS "watoots: WATOOTS_BUILD_CAPI=OFF, headers only")
  return()
endif()

find_program(WATOOTS_CARGO cargo REQUIRED)

# Debug builds the dev profile; everything else builds release. A multi-config
# generator resolves this at configure time rather than per-build -- documented
# in ADR-0005 rather than silently wrong.
if(CMAKE_BUILD_TYPE STREQUAL "Debug")
  set(watoots_cargo_flags "")
  set(watoots_cargo_dir debug)
else()
  set(watoots_cargo_flags --release)
  set(watoots_cargo_dir release)
endif()

set(watoots_staticlib
    ${PROJECT_SOURCE_DIR}/target/${watoots_cargo_dir}/libwatoots_capi.a)

# Enough to know *whether* to re-run cargo. Cargo itself decides what actually
# needs rebuilding; modelling Rust's dependency graph in CMake would be a worse
# copy of something that is already correct.
file(GLOB_RECURSE watoots_rust_sources CONFIGURE_DEPENDS
     ${PROJECT_SOURCE_DIR}/crates/*/src/*.rs
     ${PROJECT_SOURCE_DIR}/crates/*/Cargo.toml
     ${PROJECT_SOURCE_DIR}/crates/*/build.rs)

# Build the Rust side for the same deployment target CMake links against.
# Without this, cargo's C dependencies (zstd and friends, via cc-rs) pick up the
# host SDK version while rustc uses its own default, and every link warns once
# per object file that the two disagree.
if(APPLE AND CMAKE_OSX_DEPLOYMENT_TARGET)
  set(watoots_cargo_env ${CMAKE_COMMAND} -E env
                        MACOSX_DEPLOYMENT_TARGET=${CMAKE_OSX_DEPLOYMENT_TARGET})
endif()

add_custom_command(
  OUTPUT ${watoots_staticlib}
  COMMAND ${watoots_cargo_env} ${WATOOTS_CARGO} build -p watoots-capi
          ${watoots_cargo_flags}
  WORKING_DIRECTORY ${PROJECT_SOURCE_DIR}
  DEPENDS ${watoots_rust_sources}
  COMMENT "cargo build -p watoots-capi (${watoots_cargo_dir})"
  VERBATIM)

add_custom_target(watoots_capi_cargo DEPENDS ${watoots_staticlib})

# A Rust staticlib does not carry its own system dependencies, so we name them.
if(APPLE)
  set(watoots_system_libs "-framework CoreFoundation" "-framework Security"
                          "-framework SystemConfiguration")
elseif(WIN32)
  set(watoots_system_libs ws2_32 bcrypt advapi32 userenv ntdll)
else()
  set(watoots_system_libs pthread dl m)
endif()

add_dependencies(watoots_capi watoots_capi_cargo)
target_link_libraries(watoots_capi INTERFACE ${watoots_staticlib}
                                             ${watoots_system_libs})

# ---------------------------------------------------------------------------
# Install / package
# ---------------------------------------------------------------------------
include(GNUInstallDirs)
include(CMakePackageConfigHelpers)

install(FILES ${CMAKE_CURRENT_SOURCE_DIR}/include/watoots.h
              ${CMAKE_CURRENT_SOURCE_DIR}/include/watoots.hpp
        DESTINATION ${CMAKE_INSTALL_INCLUDEDIR})
install(FILES ${watoots_staticlib} DESTINATION ${CMAKE_INSTALL_LIBDIR})

# The exported target is written by hand rather than through install(EXPORT):
# the build-tree target links an absolute path into the cargo output directory,
# which is exactly what must not survive into an installed package.
set(WATOOTS_SYSTEM_LIBS "${watoots_system_libs}")
configure_package_config_file(
  ${PROJECT_SOURCE_DIR}/cmake/watootsConfig.cmake.in
  ${CMAKE_CURRENT_BINARY_DIR}/watootsConfig.cmake
  INSTALL_DESTINATION ${CMAKE_INSTALL_LIBDIR}/cmake/watoots)

write_basic_package_version_file(
  ${CMAKE_CURRENT_BINARY_DIR}/watootsConfigVersion.cmake
  VERSION ${PROJECT_VERSION}
  COMPATIBILITY SameMinorVersion)

install(FILES ${CMAKE_CURRENT_BINARY_DIR}/watootsConfig.cmake
              ${CMAKE_CURRENT_BINARY_DIR}/watootsConfigVersion.cmake
        DESTINATION ${CMAKE_INSTALL_LIBDIR}/cmake/watoots)
