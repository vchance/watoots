// Compile-only. The shipped C++ header must be self-contained (it pulls in
// watoots.h itself) and valid at the C++20 floor declared in ADR-0003.

#include "watoots.hpp"

namespace {

[[maybe_unused]] wt::Result<int> Probe() { return 0; }

[[maybe_unused]] wt::Result<void> ProbeVoid() {
  return wt::unexpected(wt::Error(WT_ERR_TRAP, "probe"));
}

}  // namespace
