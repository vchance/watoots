# The C++ host application. Links only watoots::capi -- no Rust in sight from
# here, which is the point of the exercise.
add_executable(host_cpp main.cc)
target_link_libraries(host_cpp PRIVATE watoots::capi)
target_compile_features(host_cpp PRIVATE cxx_std_20)
target_compile_options(host_cpp PRIVATE ${WATOOTS_STRICT_FLAGS})

# Run the host against whichever sample plugins have been built. They are
# skipped rather than failing when absent: each needs a different guest
# toolchain, and `tools/build-plugins.sh` builds what it can.
foreach(sample rust js py)
  string(REPLACE "-" "_" sample_file "${sample}_lint")
  set(component
      ${PROJECT_SOURCE_DIR}/examples/plugins/${sample}-lint/${sample_file}.wasm)
  if(EXISTS ${component})
    add_test(
      NAME host_cpp.${sample}
      COMMAND host_cpp ${component}
              ${PROJECT_SOURCE_DIR}/examples/policies/${sample}-lint.toml)
    set_tests_properties(
      host_cpp.${sample}
      PROPERTIES PASS_REGULAR_EXPRESSION "unresolved TODO")
  endif()
endforeach()
