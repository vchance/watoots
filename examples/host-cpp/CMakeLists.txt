# The C++ host application. Links only watoots::capi -- no Rust in sight from
# here, which is the point of the exercise.
add_executable(host_cpp main.cc)
target_link_libraries(host_cpp PRIVATE watoots::capi)
target_compile_features(host_cpp PRIVATE cxx_std_20)
target_compile_options(host_cpp PRIVATE ${WATOOTS_STRICT_FLAGS})
