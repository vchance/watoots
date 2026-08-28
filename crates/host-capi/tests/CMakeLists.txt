include(FetchContent)

FetchContent_Declare(
  googletest
  GIT_REPOSITORY https://github.com/google/googletest.git
  GIT_TAG 063de7e9578f82b369302001269680b4b1553359 # v1.18.0
  GIT_SHALLOW FALSE
  SYSTEM
  EXCLUDE_FROM_ALL)
FetchContent_MakeAvailable(googletest)

include(GoogleTest)

set(WATOOTS_CAPI_TEST_SOURCES result_test.cc owned_handle_test.cc)

# Ask whether this toolchain's standard library actually has std::expected at
# C++23 rather than assuming it: libstdc++ gained it in 13 and libc++ in 16, so
# a compiler can accept -std=c++23 and still not have the header.
try_compile(
  WATOOTS_HAS_STD_EXPECTED
  SOURCE_FROM_CONTENT
  probe_expected.cc
  "#include <version>
#if !defined(__cpp_lib_expected) || __cpp_lib_expected < 202202L
#error no std::expected
#endif
int main() { return 0; }"
  CXX_STANDARD 23
  CXX_STANDARD_REQUIRED TRUE)

# The suite runs once per way a consumer can compile the header, because
# wt::Result is a different type in each and only running one of them would
# leave the others uncovered:
#
#   cxx20       the declared floor -- std::expected does not exist here, so
#               this is the configuration that actually exercises the shim
#   cxx23       what a modern consumer gets: Result is std::expected
#   cxx23_shim  a C++23 consumer opting out, which is the supported fix for a
#               project mixing standards across translation units
#
# Each target is told, independently of the header, which backend it should end
# up with; Result.BackendMatchesTheBuildConfiguration checks the header agreed.
set(watoots_test_variants cxx20)
if(cxx_std_23 IN_LIST CMAKE_CXX_COMPILE_FEATURES)
  list(APPEND watoots_test_variants cxx23)
  if(WATOOTS_HAS_STD_EXPECTED)
    list(APPEND watoots_test_variants cxx23_shim)
  endif()
endif()
message(STATUS "watoots: C++ test variants: ${watoots_test_variants}")

foreach(variant IN LISTS watoots_test_variants)
  set(force_shim OFF)
  set(expect_std_expected 0)
  if(variant STREQUAL "cxx20")
    set(standard 20)
  elseif(variant STREQUAL "cxx23")
    set(standard 23)
    if(WATOOTS_HAS_STD_EXPECTED)
      set(expect_std_expected 1)
    endif()
  else()
    set(standard 23)
    set(force_shim ON)
  endif()

  set(target watoots_capi_${variant}_test)
  add_executable(${target} ${WATOOTS_CAPI_TEST_SOURCES})
  target_link_libraries(${target} PRIVATE watoots::capi GTest::gtest_main)
  target_compile_features(${target} PRIVATE cxx_std_${standard})
  target_compile_options(${target} PRIVATE ${WATOOTS_STRICT_FLAGS})
  target_compile_definitions(
    ${target} PRIVATE WATOOTS_TEST_EXPECT_STD_EXPECTED=${expect_std_expected})
  if(force_shim)
    target_compile_definitions(${target} PRIVATE WATOOTS_FORCE_RESULT_SHIM)
  endif()
  gtest_discover_tests(${target} TEST_PREFIX ${variant}.)
endforeach()

# Header hygiene. The cheapest test in the suite and the one most likely to
# catch a real regression: a public C API that stops compiling as C, or stops
# compiling at its own declared C++ floor. Compile-only, never linked.
add_library(watoots_abi_c11 OBJECT abi_c11_compile.c)
target_link_libraries(watoots_abi_c11 PRIVATE watoots::capi)
target_compile_features(watoots_abi_c11 PRIVATE c_std_11)
target_compile_options(watoots_abi_c11 PRIVATE ${WATOOTS_STRICT_FLAGS})

add_library(watoots_abi_cxx20 OBJECT abi_cxx20_compile.cc)
target_link_libraries(watoots_abi_cxx20 PRIVATE watoots::capi)
target_compile_features(watoots_abi_cxx20 PRIVATE cxx_std_20)
target_compile_options(watoots_abi_cxx20 PRIVATE ${WATOOTS_STRICT_FLAGS})

# The API test links the Rust staticlib, so it is built once at the C++20 floor
# rather than once per Result backend: it is testing the boundary, not the shim.
if(WATOOTS_BUILD_CAPI)
  add_executable(watoots_capi_api_test api_test.cc)
  target_link_libraries(watoots_capi_api_test PRIVATE watoots::capi
                                                      GTest::gtest_main)
  target_compile_features(watoots_capi_api_test PRIVATE cxx_std_20)
  target_compile_options(watoots_capi_api_test PRIVATE ${WATOOTS_STRICT_FLAGS})
  gtest_discover_tests(watoots_capi_api_test TEST_PREFIX capi.)
endif()
