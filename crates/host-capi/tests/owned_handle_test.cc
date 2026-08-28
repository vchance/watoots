#include <utility>

#include <gtest/gtest.h>

#include "watoots.hpp"

namespace {

// Stands in for an opaque wt_* handle until the real C API lands in M3.
struct Widget {
  int id = 0;
};

int g_deleted = 0;

struct WidgetDeleter {
  void operator()(Widget* widget) const noexcept {
    ++g_deleted;
    delete widget;
  }
};

using OwnedWidget = wt::internal::OwnedHandle<Widget, WidgetDeleter>;

OwnedWidget MakeWidget(int id) { return OwnedWidget(new Widget{id}); }

// Routed through a function so self-assignment does not trip -Wself-move.
void MoveInto(OwnedWidget& dst, OwnedWidget& src) { dst = std::move(src); }

class OwnedHandleTest : public ::testing::Test {
 protected:
  void SetUp() override { g_deleted = 0; }
};

TEST_F(OwnedHandleTest, DefaultConstructsEmpty) {
  const OwnedWidget handle;
  EXPECT_EQ(handle.Get(), nullptr);
  EXPECT_FALSE(static_cast<bool>(handle));
}

TEST_F(OwnedHandleTest, DeletesOnScopeExit) {
  {
    const OwnedWidget handle = MakeWidget(1);
    EXPECT_TRUE(static_cast<bool>(handle));
    EXPECT_EQ(g_deleted, 0);
  }
  EXPECT_EQ(g_deleted, 1);
}

TEST_F(OwnedHandleTest, EmptyHandleDeletesNothing) {
  {
    const OwnedWidget handle;
  }
  EXPECT_EQ(g_deleted, 0);
}

TEST_F(OwnedHandleTest, MoveConstructionTransfersOwnership) {
  OwnedWidget source = MakeWidget(2);
  const Widget* raw = source.Get();

  const OwnedWidget moved(std::move(source));
  EXPECT_EQ(moved.Get(), raw);
  // Reading the moved-from handle is the postcondition under test: the move
  // must null the source rather than leave it aliasing the same pointer.
  // NOLINTNEXTLINE(bugprone-use-after-move,clang-analyzer-cplusplus.Move)
  EXPECT_EQ(source.Get(), nullptr);
  EXPECT_EQ(g_deleted, 0);
}

TEST_F(OwnedHandleTest, MoveAssignmentDeletesTheOverwrittenHandle) {
  OwnedWidget target = MakeWidget(3);
  OwnedWidget source = MakeWidget(4);
  const Widget* raw = source.Get();

  target = std::move(source);
  EXPECT_EQ(g_deleted, 1);  // the old target only
  EXPECT_EQ(target.Get(), raw);
}

TEST_F(OwnedHandleTest, SelfMoveAssignmentKeepsTheHandle) {
  OwnedWidget handle = MakeWidget(5);
  const Widget* raw = handle.Get();

  MoveInto(handle, handle);
  EXPECT_EQ(handle.Get(), raw);
  EXPECT_EQ(g_deleted, 0);
}

TEST_F(OwnedHandleTest, ReleaseSuppressesTheDelete) {
  Widget* raw = nullptr;
  {
    OwnedWidget handle = MakeWidget(6);
    raw = handle.Release();
    EXPECT_EQ(handle.Get(), nullptr);
  }
  EXPECT_EQ(g_deleted, 0);

  WidgetDeleter{}(raw);
  EXPECT_EQ(g_deleted, 1);
}

TEST_F(OwnedHandleTest, ResetDeletesThePreviousHandle) {
  OwnedWidget handle = MakeWidget(7);
  handle.Reset(new Widget{8});
  EXPECT_EQ(g_deleted, 1);
  EXPECT_TRUE(static_cast<bool>(handle));

  handle.Reset();
  EXPECT_EQ(g_deleted, 2);
  EXPECT_FALSE(static_cast<bool>(handle));
}

}  // namespace
