// The C++ API over the real C library.
//
// The other test targets in this directory are header-only. This one links the
// Rust staticlib, so it is the test that says the boundary actually works
// rather than merely compiles.

#include <cstring>
#include <filesystem>
#include <fstream>
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

  // Inspect answers "what can it do": a capability row, not an interface name.
  auto report = host.Inspect(AsBytes(wasm));
  ASSERT_TRUE(report.has_value()) << report.error().Message();
  EXPECT_NE(report->find("capabilities"), std::string::npos) << *report;
  EXPECT_NE(report->find("network"), std::string::npos) << *report;
  EXPECT_NE(report->find("DENY"), std::string::npos) << *report;
}

TEST(CApi, InspectImportsListsTheInterfaces) {
  const wt::Host host = BuildHost();
  const std::string wasm = kWantsNetwork;

  auto report = host.InspectImports(AsBytes(wasm));
  ASSERT_TRUE(report.has_value()) << report.error().Message();
  EXPECT_NE(report->find("wasi:sockets/tcp"), std::string::npos) << *report;
  EXPECT_NE(report->find("permissions.net"), std::string::npos) << *report;
}

TEST(CApi, CheckTargetsRejectsAWorldTheComponentDoesNotImplement) {
  const wt::Host host = BuildHost();
  const std::string wasm = kSelfContained;

  // Written next to the binary so the test needs no fixture on disk.
  const std::filesystem::path wit =
      std::filesystem::temp_directory_path() / "watoots_capi_targets.wit";
  {
    std::ofstream out(wit);
    out << "package test:other@0.1.0;\n"
        << "world formatter {\n  export format: func() -> string;\n}\n";
  }

  auto checked = host.CheckTargets(AsBytes(wasm), wit.string(), "formatter");
  std::filesystem::remove(wit);

  ASSERT_FALSE(checked.has_value());
  EXPECT_EQ(checked.error().Code(), WT_ERR_LOAD);
  EXPECT_NE(checked.error().Message().find("does not implement world"),
            std::string::npos)
      << checked.error().Message();
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

// A component that imports wasi:logging and calls it once, at `warn`. The
// index-based instance type is not stylistic: an instance type used as an
// import may only reference types it also exports, and the text format has no
// way to bind a name to the exported one. See crates/host/tests/logging.rs.
constexpr const char* kLogsOnce = R"(
(component
  (type (;0;)
    (instance
      (type (;0;) (enum "trace" "debug" "info" "warn" "error" "critical"))
      (export (;1;) "level" (type (eq 0)))
      (type (;2;) (func (param "level" 1) (param "context" string) (param "message" string)))
      (export (;0;) "log" (func (type 2)))
    )
  )
  (import "wasi:logging/logging@0.1.0-draft" (instance $log (type 0)))
  (alias export $log "log" (func $log_fn))

  (core module $libc
    (memory (export "memory") 1)
    (func (export "realloc") (param i32 i32 i32 i32) (result i32)
      i32.const 256)
  )
  (core instance $libc_i (instantiate $libc))
  (core func $log_lowered
    (canon lower (func $log_fn)
      (memory $libc_i "memory")
      (realloc (func $libc_i "realloc"))))

  (core module $m
    (import "log" "log" (func $log (param i32 i32 i32 i32 i32)))
    (import "libc" "memory" (memory 1))
    (data (i32.const 0) "boot")
    (data (i32.const 8) "config is malformed")
    (func (export "run")
      (call $log (i32.const 3) (i32.const 0) (i32.const 4) (i32.const 8) (i32.const 19)))
  )
  (core instance $log_i (export "log" (func $log_lowered)))
  (core instance $i (instantiate $m
    (with "log" (instance $log_i))
    (with "libc" (instance $libc_i))))

  (func $run (canon lift (core func $i "run")))
  (export "run" (func $run))
)
)";

TEST(CApi, ALoggingPluginReachesTheApplicationSink) {
  std::vector<std::string> lines;

  wt::HostBuilder builder;
  ASSERT_TRUE(builder.ManifestFromString("[permissions]\nlogging = \"info\"\n")
                  .has_value());
  ASSERT_TRUE(
      builder
          .LogSink([&lines](wt_log_level level, std::string_view context,
                            std::string_view message) {
            lines.emplace_back(std::string(wt_log_level_name(level)) + " " +
                               std::string(context) + ": " +
                               std::string(message));
          })
          .has_value());

  auto host = builder.Build();
  ASSERT_TRUE(host.has_value()) << host.error().Message();

  const std::string wasm = kLogsOnce;
  auto plugin = host->LoadBinary("talker", AsBytes(wasm));
  ASSERT_TRUE(plugin.has_value()) << plugin.error().Message();

  auto result = plugin->Call("run");
  ASSERT_TRUE(result.has_value()) << result.error().Message();

  ASSERT_EQ(lines.size(), 1U);
  EXPECT_EQ(lines.front(), "warn boot: config is malformed");
}

TEST(CApi, LoggingIsRefusedWhenTheManifestDoesNotGrantIt) {
  // The manifest decides, not the sink: a registered sink does not make an
  // ungranted import loadable.
  wt::HostBuilder builder;
  ASSERT_TRUE(builder.ManifestFromString("").has_value());
  ASSERT_TRUE(builder
                  .LogSink([](wt_log_level, std::string_view,
                              std::string_view) { FAIL(); })
                  .has_value());
  auto host = builder.Build();
  ASSERT_TRUE(host.has_value()) << host.error().Message();

  const std::string wasm = kLogsOnce;
  auto plugin = host->LoadBinary("talker", AsBytes(wasm));
  ASSERT_FALSE(plugin.has_value());
  EXPECT_EQ(plugin.error().Code(), WT_ERR_PERMISSION_DENIED);
  EXPECT_NE(plugin.error().Message().find("permissions.logging"),
            std::string::npos)
      << plugin.error().Message();
}

TEST(CApi, TheLevelCeilingFiltersBeforeTheSinkSeesAnything) {
  int calls = 0;

  wt::HostBuilder builder;
  ASSERT_TRUE(builder.ManifestFromString("[permissions]\nlogging = \"error\"\n")
                  .has_value());
  ASSERT_TRUE(builder
                  .LogSink([&calls](wt_log_level, std::string_view,
                                    std::string_view) { ++calls; })
                  .has_value());
  auto host = builder.Build();
  ASSERT_TRUE(host.has_value()) << host.error().Message();

  const std::string wasm = kLogsOnce;
  auto plugin = host->LoadBinary("talker", AsBytes(wasm));
  ASSERT_TRUE(plugin.has_value()) << plugin.error().Message();
  ASSERT_TRUE(plugin->Call("run").has_value());

  // The plugin logged at `warn`; the manifest admits `error` and above.
  EXPECT_EQ(calls, 0);
}

TEST(CApi, TheLogVolumeCeilingIsReportedAsALimit) {
  wt::HostBuilder builder;
  // "boot" + "config is malformed" is 23 bytes, so one message does not fit.
  ASSERT_TRUE(builder
                  .ManifestFromString("[permissions]\nlogging = \"trace\"\n\n"
                                      "[limits]\nlog_bytes = 8\n")
                  .has_value());
  ASSERT_TRUE(builder
                  .LogSink([](wt_log_level, std::string_view,
                              std::string_view) { FAIL(); })
                  .has_value());
  auto host = builder.Build();
  ASSERT_TRUE(host.has_value()) << host.error().Message();

  const std::string wasm = kLogsOnce;
  auto plugin = host->LoadBinary("firehose", AsBytes(wasm));
  ASSERT_TRUE(plugin.has_value()) << plugin.error().Message();

  auto result = plugin->Call("run");
  ASSERT_FALSE(result.has_value());
  EXPECT_EQ(result.error().Code(), WT_ERR_LIMIT_EXCEEDED);
  EXPECT_NE(result.error().Message().find("limits.log_bytes"),
            std::string::npos)
      << result.error().Message();
}

TEST(CApi, LogLevelNamesMatchTheWitSpelling) {
  EXPECT_STREQ(wt_log_level_name(WT_LOG_TRACE), "trace");
  EXPECT_STREQ(wt_log_level_name(WT_LOG_WARN), "warn");
  EXPECT_STREQ(wt_log_level_name(WT_LOG_CRITICAL), "critical");
}

TEST(CApi, ANullLogSinkIsRejected) {
  wt_error_t* error = nullptr;
  wt_host_builder_t* builder = wt_host_builder_new();
  EXPECT_EQ(wt_host_builder_log_sink(builder, nullptr, nullptr, &error),
            WT_ERR_INVALID_ARGUMENT);
  ASSERT_NE(error, nullptr);
  wt_error_delete(error);
  wt_host_builder_delete(builder);
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

TEST(CApi, PluginStatsAreObservedNotReported) {
  const wt::Host host = BuildHost();
  const std::string wasm = kSelfContained;

  auto plugin = host.LoadBinary("answer", AsBytes(wasm));
  ASSERT_TRUE(plugin.has_value()) << plugin.error().Message();

  auto before = plugin->Stats();
  ASSERT_TRUE(before.has_value()) << before.error().Message();
  EXPECT_EQ(before->calls, 0U);

  auto result = plugin->Call("answer");
  ASSERT_TRUE(result.has_value()) << result.error().Message();

  auto after = plugin->Stats();
  ASSERT_TRUE(after.has_value()) << after.error().Message();
  EXPECT_EQ(after->calls, 1U);
  EXPECT_EQ(after->imports_denied, 0U);
}

// ---------------------------------------------------------------------------
// Profiling (ADR-0009)
// ---------------------------------------------------------------------------

// A host with profiling on. Not folded into BuildHost: every other test in this
// file is meant to run on the unprofiled path, which is the default one.
wt::Host BuildProfiledHost(uint64_t sample_interval_ms = 0) {
  wt::HostBuilder builder;
  if (sample_interval_ms == 0) {
    EXPECT_TRUE(builder.Profile().has_value());
  } else {
    EXPECT_TRUE(builder.ProfileGuestSamples(sample_interval_ms).has_value());
  }
  auto host = builder.Build();
  EXPECT_TRUE(host.has_value());
  return std::move(host).value();
}

TEST(CApi, ProfilingIsRefusedUntilItIsAskedFor) {
  const wt::Host host = BuildHost();
  const std::string wasm = kSelfContained;

  auto plugin = host.LoadBinary("answer", AsBytes(wasm));
  ASSERT_TRUE(plugin.has_value()) << plugin.error().Message();

  auto profile = plugin->Profile();
  ASSERT_FALSE(profile.has_value());
  EXPECT_EQ(profile.error().Code(), WT_ERR_INVALID_ARGUMENT);
}

TEST(CApi, ProfileSplitsTimeAtTheBoundary) {
  const wt::Host host = BuildProfiledHost();
  const std::string wasm = kSelfContained;

  auto plugin = host.LoadBinary("answer", AsBytes(wasm));
  ASSERT_TRUE(plugin.has_value()) << plugin.error().Message();

  auto called = plugin->Call("answer");
  ASSERT_TRUE(called.has_value()) << called.error().Message();

  auto profile = plugin->Profile();
  ASSERT_TRUE(profile.has_value()) << profile.error().Message();
  EXPECT_EQ(profile->calls, 1U);
  EXPECT_GT(profile->wall_nanos, 0U);
  // Marshalling is defined as the remainder, so the three always add up to the
  // wall time. That is the definition rather than a measurement, and this pins
  // the definition.
  EXPECT_EQ(
      profile->guest_nanos + profile->host_nanos + profile->marshalling_nanos,
      profile->wall_nanos);

  ASSERT_EQ(profile->functions.size(), 1U);
  const wt::FunctionProfile& row = profile->functions.front();
  EXPECT_EQ(row.kind, WT_FUNCTION_EXPORT);
  EXPECT_EQ(row.func, "answer");
  EXPECT_EQ(row.interface_name, "");
  EXPECT_EQ(row.calls, 1U);
}

// Straight against the C API, because the C++ accessor never asks for a row it
// was not told exists -- and the bounds check is the thing being tested.
TEST(CApi, AProfileRowOutOfRangeIsNotFound) {
  wt_error_t* error = nullptr;
  wt_host_builder_t* builder = wt_host_builder_new();
  ASSERT_EQ(wt_host_builder_profile(builder, &error), WT_OK);

  wt_host_t* host = nullptr;
  ASSERT_EQ(wt_host_builder_build(builder, &host, &error), WT_OK);
  wt_host_builder_delete(builder);

  const std::string wasm = kSelfContained;
  wt_plugin_t* plugin = nullptr;
  ASSERT_EQ(wt_host_load_binary(
                host, "answer",
                // NOLINTNEXTLINE(cppcoreguidelines-pro-type-reinterpret-cast)
                reinterpret_cast<const uint8_t*>(wasm.data()), wasm.size(),
                &plugin, &error),
            WT_OK);

  wt_plugin_profile_t totals{};
  ASSERT_EQ(wt_plugin_profile(plugin, &totals, &error), WT_OK);
  EXPECT_EQ(totals.function_count, 0U);

  wt_function_profile_t row{};
  EXPECT_EQ(wt_plugin_profile_function(plugin, 0, &row, &error),
            WT_ERR_NOT_FOUND);
  ASSERT_NE(error, nullptr);
  wt_error_delete(error);

  wt_plugin_delete(plugin);
  wt_host_delete(host);
}

TEST(CApi, GuestSamplesWriteAFirefoxProfile) {
  const wt::Host host = BuildProfiledHost(1);
  const std::string wasm = kSelfContained;

  auto plugin = host.LoadBinary("answer", AsBytes(wasm));
  ASSERT_TRUE(plugin.has_value()) << plugin.error().Message();
  ASSERT_TRUE(plugin->Call("answer").has_value());

  const std::filesystem::path json =
      std::filesystem::temp_directory_path() / "watoots_capi_guest.json";
  auto wrote = plugin->WriteGuestProfile(json.string());
  ASSERT_TRUE(wrote.has_value()) << wrote.error().Message();

  std::ifstream in(json);
  std::string first;
  std::getline(in, first);
  ASSERT_FALSE(first.empty());
  EXPECT_EQ(first.front(), '{');
  in.close();
  std::filesystem::remove(json);

  // The profiler is consumed by writing it, and says so rather than quietly
  // producing an empty second file.
  EXPECT_FALSE(plugin->WriteGuestProfile(json.string()).has_value());
}

TEST(CApi, AZeroSampleIntervalIsRejected) {
  wt::HostBuilder builder;
  auto refused = builder.ProfileGuestSamples(0);
  ASSERT_FALSE(refused.has_value());
  EXPECT_EQ(refused.error().Code(), WT_ERR_INVALID_ARGUMENT);
}
