# The third C++ host: a file previewer whose format decoders are plugins. The
# codec is on the *untrusted* side here -- the opposite of host-cpp-asset, and
# the trade this example exists to show. Its own binary for the same reason the
# other two are.

# stb, for writing PNG. The FetchContent declaration lives in host-cpp-asset and
# populates `watoots_stb` for the whole tree; this host only links it.
add_executable(host_cpp_preview main.cc stb_impl.cc)
target_link_libraries(host_cpp_preview PRIVATE watoots::capi watoots_stb)
target_include_directories(host_cpp_preview PRIVATE ${PROJECT_SOURCE_DIR}/examples/common)
target_compile_features(host_cpp_preview PRIVATE cxx_std_20)
target_compile_options(host_cpp_preview PRIVATE ${WATOOTS_STRICT_FLAGS})

set(fixtures ${PROJECT_SOURCE_DIR}/examples/fixtures/preview)

# Every built decoder, through the same four inputs. The bomb is the test that
# matters: it has to *fail*, cleanly, naming the ceiling, with the process
# exiting normally -- which is what a real viewer would need from it.
foreach(sample rust cpp)
  set(component ${PROJECT_SOURCE_DIR}/examples/plugins/${sample}-qoi/${sample}_qoi.wasm)
  set(policy ${PROJECT_SOURCE_DIR}/examples/policies/${sample}-qoi.toml)
  if(EXISTS ${component})
    add_test(
      NAME host_cpp_preview.${sample}.decodes
      COMMAND host_cpp_preview ${policy} ${fixtures}/blocks.qoi
              ${CMAKE_CURRENT_BINARY_DIR}/${sample}-blocks.png ${component})
    set_tests_properties(host_cpp_preview.${sample}.decodes
                         PROPERTIES PASS_REGULAR_EXPRESSION "wrote .*32x32 RGBA8 via qoi")

    add_test(
      NAME host_cpp_preview.${sample}.bomb
      COMMAND host_cpp_preview ${policy} ${fixtures}/bomb.qoi
              ${CMAKE_CURRENT_BINARY_DIR}/${sample}-bomb.png ${component})
    # The reason on stderr is the ceiling by name, and nothing was written.
    # PASS_REGULAR_EXPRESSION makes CTest judge by output rather than exit
    # code, which is what is wanted here: a crash or a kill prints no such
    # line, so the regex alone tells a clean refusal from all three.
    set_tests_properties(
      host_cpp_preview.${sample}.bomb
      PROPERTIES PASS_REGULAR_EXPRESSION "WT_ERR_LIMIT_EXCEEDED.*limits.memory"
                 FAIL_REGULAR_EXPRESSION "wrote ")

    add_test(
      NAME host_cpp_preview.${sample}.truncated
      COMMAND host_cpp_preview ${policy} ${fixtures}/truncated.qoi
              ${CMAKE_CURRENT_BINARY_DIR}/${sample}-truncated.png ${component})
    set_tests_properties(host_cpp_preview.${sample}.truncated
                         PROPERTIES PASS_REGULAR_EXPRESSION "truncated"
                                    FAIL_REGULAR_EXPRESSION "wrote ")

    add_test(
      NAME host_cpp_preview.${sample}.not_this_format
      COMMAND host_cpp_preview ${policy} ${fixtures}/not-qoi.bin
              ${CMAKE_CURRENT_BINARY_DIR}/${sample}-other.png ${component})
    set_tests_properties(host_cpp_preview.${sample}.not_this_format
                         PROPERTIES PASS_REGULAR_EXPRESSION "no installed decoder"
                                    FAIL_REGULAR_EXPRESSION "wrote ")
  endif()
endforeach()

# Two decoders installed at once, so dispatch is real and not a loop of one.
set(rust_qoi ${PROJECT_SOURCE_DIR}/examples/plugins/rust-qoi/rust_qoi.wasm)
set(cpp_qoi ${PROJECT_SOURCE_DIR}/examples/plugins/cpp-qoi/cpp_qoi.wasm)
if(EXISTS ${rust_qoi} AND EXISTS ${cpp_qoi})
  add_test(
    NAME host_cpp_preview.two_decoders
    COMMAND host_cpp_preview ${PROJECT_SOURCE_DIR}/examples/policies/rust-qoi.toml
            ${fixtures}/blocks.qoi ${CMAKE_CURRENT_BINARY_DIR}/two.png ${cpp_qoi} ${rust_qoi})
  set_tests_properties(host_cpp_preview.two_decoders
                       PROPERTIES PASS_REGULAR_EXPRESSION "wrote .*via qoi")
endif()
