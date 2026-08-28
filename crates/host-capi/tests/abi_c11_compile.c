// Compile-only. The shipped C header must be valid C11 with no warnings under
// -Wall -Wextra -Wpedantic -Werror; a C-only consumer must never need a C++
// compiler. Not linked -- M3 supplies the implementations.

#include "watoots.h"

wt_status watoots_abi_c11_probe(wt_host_t* host);

wt_status watoots_abi_c11_probe(wt_host_t* host) {
  (void)host;
  (void)wt_version_string;
  (void)wt_status_name;
  return WT_OK;
}
