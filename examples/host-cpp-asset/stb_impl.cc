// The stb implementations, alone in their own translation unit.
//
// stb is a header-only library whose implementation is compiled by defining a
// macro before the include. Doing that here rather than in main.cc keeps
// third-party code in a file that contains nothing else: the CMake target
// pulls stb in with SYSTEM, so every diagnostic these two headers would raise
// -- under -Wall -Wextra -Wpedantic -Werror and under clang-tidy alike -- is
// suppressed by -isystem at its own source location, and no NOLINT is needed
// or wanted in ours.

#define STB_IMAGE_IMPLEMENTATION
#define STB_IMAGE_WRITE_IMPLEMENTATION

// PNG only, on both sides. The world's contract is RGB8 pixels; a host that
// quietly accepted a JPEG would be advertising a codec this example does not
// test, and linking four more decoders to prove it does not is not free.
#define STBI_ONLY_PNG

#include "stb_image.h"
#include "stb_image_write.h"
