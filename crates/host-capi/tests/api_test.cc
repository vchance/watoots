// The C++ API over the real C library.
//
// The other test targets in this directory are header-only. This one links the
// Rust staticlib, so it is the test that says the boundary actually works
// rather than merely compiles.

#include <cstring>
#include <optional>
#include <span>
#include <string>
#include <vector>

#include <gtest/gtest.h>

#include "watoots.hpp"

namespace {

// Components are WAT text: wasmtime accepts it, so the tests stay readable and
// need no guest toolchain.
std::span<const std::byte> AsBytes(const std::string& text) {
  // Viewing bytes as bytes. ADR-0003 keeps this check on so each such cast has
  // to be justified in place rather than disappearing into a blanket exemption.
  // NOLINTNEXTLINE(cppcoreguidelines-pro-type-reinterpret-cast)
  return {reinterpret_cast<const std::byte*>(text.data()), text.size()};
}

constexpr const char* kSelfContained = R"(
(component
  (core module $m
    (func (export "answer") (result i32) i32.const 42))
  (core instance $i (instantiate $m))
  (func $answer (result s32) (canon lift (core func $i "answer")))
  (export "answer" (func $answer))
)
)";

constexpr const char* kWantsNetwork = R"(
(component
  (import "wasi:sockets/tcp@0.2.6" (instance (export "connect" (func))))
)
)";

wt::Host BuildHost(const char* manifest = "") {
  wt::HostBuilder builder;
  EXPECT_TRUE(builder.ManifestFromString(manifest).has_value());
  auto host = builder.Build();
  EXPECT_TRUE(host.has_value());
  return std::move(host).value();
}

TEST(CApi, ReportsItsVersion) {
  EXPECT_STRNE(wt_version_string(), "");
  EXPECT_STREQ(wt_status_name(WT_OK), "WT_OK");
  EXPECT_STREQ(wt_status_name(WT_ERR_MANIFEST), "WT_ERR_MANIFEST");
}

TEST(CApi, LoadsAndCallsAComponent) {
  const wt::Host host = BuildHost();
  const std::string wasm = kSelfContained;

  auto plugin = host.LoadBinary("answer", AsBytes(wasm));
  ASSERT_TRUE(plugin.has_value()) << plugin.error().Message();
  EXPECT_EQ(plugin->Name(), "answer");

  auto result = plugin->Call("answer");
  ASSERT_TRUE(result.has_value()) << result.error().Message();
  // value_or rather than value: bugprone-unchecked-optional-access cannot see
  // through gtest's fatal-assert macro, so the ASSERT above does not count as
  // proof of engagement. The assertion still carries the test; the fallback is
  // unreachable. main.cc reads the same way.
  const std::optional<std::string>& returned = *result;
  ASSERT_TRUE(returned.has_value());
  EXPECT_EQ(returned.value_or(""), "42");
}

TEST(CApi, AnUngrantedImportFailsTheLoad) {
  const wt::Host host = BuildHost();
  const std::string wasm = kWantsNetwork;

  auto plugin = host.LoadBinary("net", AsBytes(wasm));
  ASSERT_FALSE(plugin.has_value());
  EXPECT_EQ(plugin.error().Code(), WT_ERR_PERMISSION_DENIED);
  EXPECT_NE(plugin.error().Message().find("wasi:sockets/tcp"),
            std::string::npos)
      << plugin.error().Message();
}

TEST(CApi, InspectDescribesWithoutInstantiating) {
  const wt::Host host = BuildHost();
  const std::string wasm = kWantsNetwork;

  auto report = host.Inspect(AsBytes(wasm));
  ASSERT_TRUE(report.has_value()) << report.error().Message();
  EXPECT_NE(report->find("DENY"), std::string::npos) << *report;
  EXPECT_NE(report->find("permissions.net"), std::string::npos) << *report;
}

TEST(CApi, AMalformedManifestReportsAMessage) {
  wt::HostBuilder builder;
  auto applied = builder.ManifestFromString("[permissions]\nfs.raed = []\n");
  ASSERT_FALSE(applied.has_value());
  EXPECT_EQ(applied.error().Code(), WT_ERR_MANIFEST);
  EXPECT_FALSE(applied.error().Message().empty());
}

TEST(CApi, CallingAMissingExportIsNotFound) {
  const wt::Host host = BuildHost();
  const std::string wasm = kSelfContained;
  auto plugin = host.LoadBinary("answer", AsBytes(wasm));
  ASSERT_TRUE(plugin.has_value());

  auto result = plugin->Call("nope");
  ASSERT_FALSE(result.has_value());
  EXPECT_EQ(result.error().Code(), WT_ERR_NOT_FOUND);
}

TEST(CApi, FuelStopsARunawayGuest) {
  const wt::Host host = BuildHost("[limits]\nfuel = 100_000\n");
  const std::string wasm = R"(
(component
  (core module $m (func (export "spin") (loop $l br $l)))
  (core instance $i (instantiate $m))
  (func $spin (canon lift (core func $i "spin")))
  (export "spin" (func $spin))
)
)";
  auto plugin = host.LoadBinary("spinner", AsBytes(wasm));
  ASSERT_TRUE(plugin.has_value()) << plugin.error().Message();

  auto result = plugin->Call("spin");
  ASSERT_FALSE(result.has_value());
  EXPECT_EQ(result.error().Code(), WT_ERR_LIMIT_EXCEEDED);
}

TEST(CApi, NullArgumentsAreRejectedNotDereferenced) {
  wt_error_t* error = nullptr;
  EXPECT_EQ(wt_host_builder_manifest_from_file(nullptr, "x", &error),
            WT_ERR_INVALID_ARGUMENT);
  ASSERT_NE(error, nullptr);
  EXPECT_STRNE(wt_error_message(error), "");
  wt_error_delete(error);

  // A NULL error out-parameter is allowed and simply discards the message.
  EXPECT_EQ(wt_host_load(nullptr, "x", nullptr, nullptr),
            WT_ERR_INVALID_ARGUMENT);
}

TEST(CApi, ABuilderCannotBeBuiltTwice) {
  wt::HostBuilder builder;
  auto first = builder.Build();
  ASSERT_TRUE(first.has_value());

  auto second = builder.Build();
  ASSERT_FALSE(second.has_value());
  EXPECT_EQ(second.error().Code(), WT_ERR_INVALID_ARGUMENT);
}

TEST(CApi, AHostFunctionCanCaptureApplicationState) {
  // The reason host functions are std::function and not a bare pointer.
  int calls = 0;
  std::string last_message;

  wt::HostBuilder builder;
  ASSERT_TRUE(builder.ManifestFromString("").has_value());
  ASSERT_TRUE(builder
                  .HostFunc("watoots:example/log@0.1.0", "emit",
                            [&calls, &last_message](
                                std::span<const std::string_view> args)
                                -> wt::Result<wt::Value> {
                              ++calls;
                              last_message = std::string(args.back());
                              return wt::Value{};
                            })
                  .has_value());

  auto host = builder.Build();
  ASSERT_TRUE(host.has_value()) << host.error().Message();

  // Declaring the interface is enough for the grant check to pass.
  const std::string wasm = R"(
(component
  (import "watoots:example/log@0.1.0" (instance (export "emit" (func))))
)
)";
  auto report = host->Inspect(AsBytes(wasm));
  ASSERT_TRUE(report.has_value()) << report.error().Message();
  EXPECT_EQ(report->find("DENY"), std::string::npos) << *report;
  EXPECT_EQ(calls, 0);
}

TEST(CApi, MovingAPluginDoesNotDoubleFree) {
  const wt::Host host = BuildHost();
  const std::string wasm = kSelfContained;

  auto loaded = host.LoadBinary("answer", AsBytes(wasm));
  ASSERT_TRUE(loaded.has_value());

  wt::Plugin moved = std::move(loaded).value();
  EXPECT_EQ(moved.Name(), "answer");

  auto result = moved.Call("answer");
  ASSERT_TRUE(result.has_value()) << result.error().Message();
  // Destructors run here; ASan would catch a double free.
}

}  // namespace
