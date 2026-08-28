#include <string>
#include <utility>

#include <gtest/gtest.h>

#include "watoots.hpp"

namespace {

TEST(Result, HoldsAValue) {
  wt::Result<int> result = 7;
  ASSERT_TRUE(result.has_value());
  EXPECT_TRUE(static_cast<bool>(result));
  EXPECT_EQ(result.value(), 7);
  EXPECT_EQ(*result, 7);
}

TEST(Result, HoldsAnError) {
  const wt::Result<int> result =
      wt::unexpected(wt::Error(WT_ERR_MANIFEST, "unknown key: fs.exec"));
  ASSERT_FALSE(result.has_value());
  EXPECT_FALSE(static_cast<bool>(result));
  EXPECT_EQ(result.error().Code(), WT_ERR_MANIFEST);
  EXPECT_EQ(result.error().Message(), "unknown key: fs.exec");
}

TEST(Result, ValueOrFallsBackOnError) {
  const wt::Result<int> ok = 3;
  EXPECT_EQ(ok.value_or(42), 3);

  const wt::Result<int> failed =
      wt::unexpected(wt::Error(WT_ERR_INTERNAL, "unreachable"));
  EXPECT_EQ(failed.value_or(42), 42);
}

TEST(Result, CarriesAMoveOnlyPayload) {
  wt::Result<std::string> result = std::string("plugins/lint.wasm");
  ASSERT_TRUE(result.has_value());
  EXPECT_EQ(std::move(result).value(), "plugins/lint.wasm");
}

TEST(Result, ArrowReachesTheValue) {
  wt::Result<std::string> result = std::string("lint");
  ASSERT_TRUE(result.has_value());
  EXPECT_EQ(result->size(), 4U);
}

TEST(Result, VoidSpecializationSucceedsByDefault) {
  const wt::Result<void> ok;
  EXPECT_TRUE(ok.has_value());
}

TEST(Result, VoidSpecializationCarriesAnError) {
  const wt::Result<void> failed = wt::unexpected(
      wt::Error(WT_ERR_PERMISSION_DENIED, "net not granted by manifest"));
  ASSERT_FALSE(failed.has_value());
  EXPECT_EQ(failed.error().Code(), WT_ERR_PERMISSION_DENIED);
  EXPECT_EQ(failed.error().Message(), "net not granted by manifest");
}

TEST(Error, DefaultsToInternal) {
  const wt::Error error;
  EXPECT_EQ(error.Code(), WT_ERR_INTERNAL);
  EXPECT_TRUE(error.Message().empty());
}

// The header picks its backend from __cpp_lib_expected; CMake works out what
// that should be for this target independently. If they ever disagree, a
// consumer is silently getting a different wt::Result than we think we ship.
TEST(Result, BackendMatchesTheBuildConfiguration) {
  EXPECT_EQ(WATOOTS_RESULT_USES_STD_EXPECTED, WATOOTS_TEST_EXPECT_STD_EXPECTED);
}

}  // namespace
