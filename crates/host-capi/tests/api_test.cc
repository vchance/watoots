// The C++ API over the real C library.
//
// The other test targets in this directory are header-only. This one links the
// Rust staticlib, so it is the test that says the boundary actually works
// rather than merely compiles.

#include <algorithm>
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

// A counter with both reload state hooks (ADR-0010), and a second build of it
// whose `get` adds a thousand -- so one call says both which build is running
// and what state it started from.
constexpr const char* kCounterV1 = R"(
(component
  (core module $m
    (global $n (mut i32) (i32.const 0))
    (func (export "get") (result i32) (global.get $n))
    (func (export "bump") (param i32)
      (global.set $n (i32.add (global.get $n) (local.get 0))))
    (func (export "save") (result i32) (global.get $n))
    (func (export "restore") (param i32) (global.set $n (local.get 0))))
  (core instance $i (instantiate $m))
  (func $get (result u32) (canon lift (core func $i "get")))
  (func $bump (param "n" u32) (canon lift (core func $i "bump")))
  (func $save (result u32) (canon lift (core func $i "save")))
  (func $restore (param "state" u32) (canon lift (core func $i "restore")))
  (export "get" (func $get))
  (export "bump" (func $bump))
  (export "save-state" (func $save))
  (export "restore-state" (func $restore))
)
)";

constexpr const char* kCounterV2 = R"(
(component
  (core module $m
    (global $n (mut i32) (i32.const 0))
    (func (export "get") (result i32) (i32.add (global.get $n) (i32.const 1000)))
    (func (export "save") (result i32) (global.get $n))
    (func (export "restore") (param i32) (global.set $n (local.get 0))))
  (core instance $i (instantiate $m))
  (func $get (result u32) (canon lift (core func $i "get")))
  (func $save (result u32) (canon lift (core func $i "save")))
  (func $restore (param "state" u32) (canon lift (core func $i "restore")))
  (export "get" (func $get))
  (export "save-state" (func $save))
  (export "restore-state" (func $restore))
)
)";

// The same world, plus a socket the manifest never granted.
constexpr const char* kCounterWantsNetwork = R"(
(component
  (import "wasi:sockets/tcp@0.2.6" (instance (export "connect" (func))))
  (core module $m
    (global $n (mut i32) (i32.const 0))
    (func (export "get") (result i32) (global.get $n)))
  (core instance $i (instantiate $m))
  (func $get (result u32) (canon lift (core func $i "get")))
  (export "get" (func $get))
)
)";

wt::Host BuildHost(const char* manifest = "") {
  wt::HostBuilder builder;
  EXPECT_TRUE(builder.ManifestFromString(manifest).has_value());
  auto host = builder.Build();
  EXPECT_TRUE(host.has_value());
  return std::move(host).value();
}

// The public half of a throwaway openssl P-256 keypair, the same one
// crates/host/tests/fixtures/signing uses. Only its ability to *not* match
// matters here: these tests check that the C surface reports a refusal, which
// needs no valid signature at all.
constexpr const char* kSigningManifest = R"TOML(
[signature]
keys = ["""
-----BEGIN PUBLIC KEY-----
MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEzuhbtxgdBr7pXOlkrACLK8+PXkOL
WmxJSG+8X0cSE6TaYhzhil1GgjEIbsI9QoDGb1DR6/EBuxvg2E4ejBuqDg==
-----END PUBLIC KEY-----
"""]
)TOML";

TEST(CApi, ReportsItsVersion) {
  EXPECT_STRNE(wt_version_string(), "");
  EXPECT_STREQ(wt_status_name(WT_OK), "WT_OK");
  EXPECT_STREQ(wt_status_name(WT_ERR_MANIFEST), "WT_ERR_MANIFEST");
  EXPECT_STREQ(wt_status_name(WT_ERR_SIGNATURE_INVALID),
               "WT_ERR_SIGNATURE_INVALID");
}

TEST(CApi, AnUnsignedLoadIsRefusedWhenTheManifestListsKeys) {
  const wt::Host host = BuildHost(kSigningManifest);
  const std::string wasm = kSelfContained;

  // The plain path cannot carry a signature, so under a signing manifest it
  // must refuse rather than quietly load.
  auto plugin = host.LoadBinary("unsigned", AsBytes(wasm));
  ASSERT_FALSE(plugin.has_value());
  EXPECT_EQ(plugin.error().Code(), WT_ERR_SIGNATURE_INVALID);
}

TEST(CApi, ASignatureThatDoesNotVerifyIsRefusedAsASignatureProblem) {
  const wt::Host host = BuildHost(kSigningManifest);
  const std::string wasm = kSelfContained;
  const std::string signature = "bm90IGEgc2lnbmF0dXJl";

  auto plugin =
      host.LoadBinarySigned("wrong", AsBytes(wasm), AsBytes(signature));
  ASSERT_FALSE(plugin.has_value());
  // Not WT_ERR_INTERNAL: a refused signature is not a bug on our side, and the
  // catch-all in `From<ErrorKind>` used to make it look like one.
  EXPECT_EQ(plugin.error().Code(), WT_ERR_SIGNATURE_INVALID);
}

TEST(CApi, ASignatureIsIgnoredWhenNoKeysAreConfigured) {
  // Absence does not deny here, uniquely. An application may pass a signature
  // unconditionally and let the manifest decide whether it matters.
  const wt::Host host = BuildHost();
  const std::string wasm = kSelfContained;
  const std::string signature = "not even base64 !!";

  auto plugin =
      host.LoadBinarySigned("unchecked", AsBytes(wasm), AsBytes(signature));
  ASSERT_TRUE(plugin.has_value()) << plugin.error().Message();
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
  EXPECT_EQ(after->reloads, 0U);
}

// ---------------------------------------------------------------------------
// Reload (ADR-0010)
// ---------------------------------------------------------------------------

TEST(CApi, ReloadRunsTheNewCodeAndCarriesState) {
  const wt::Host host = BuildHost();
  const std::string first = kCounterV1;
  const std::string second = kCounterV2;

  auto plugin = host.LoadBinary("counter", AsBytes(first));
  ASSERT_TRUE(plugin.has_value()) << plugin.error().Message();

  const std::vector<std::string> seven{"7"};
  ASSERT_TRUE(plugin->Call("bump", seven).has_value());

  auto report = plugin->Reload(AsBytes(second));
  ASSERT_TRUE(report.has_value()) << report.error().Message();
  EXPECT_TRUE(report->state_saved);
  EXPECT_TRUE(report->state_restored);
  EXPECT_EQ(report->reloads, 1U);

  // 1007 in one number: 1000 says the new build is running, 7 says it started
  // from the old instance's state.
  auto value = plugin->Call("get");
  ASSERT_TRUE(value.has_value()) << value.error().Message();
  EXPECT_EQ(value->value_or(""), "1007");
  EXPECT_EQ(plugin->Name(), "counter")
      << "a reload replaces code, not identity";

  // Counters accumulate across a reload; the reload count is what separates
  // "one instance did all this" from "this is the second build".
  auto stats = plugin->Stats();
  ASSERT_TRUE(stats.has_value()) << stats.error().Message();
  EXPECT_EQ(stats->reloads, 1U);
  EXPECT_GE(stats->calls, 4U) << "bump, get, and the two handoff hooks";
}

TEST(CApi, AReloadThatWantsMoreIsRefusedAndTheOldCodeRunsOn) {
  // ADR-0010's central claim, from C++: new bytes must not acquire a
  // capability by arriving as an update.
  const wt::Host host = BuildHost();
  const std::string first = kCounterV1;
  const std::string greedy = kCounterWantsNetwork;

  auto plugin = host.LoadBinary("counter", AsBytes(first));
  ASSERT_TRUE(plugin.has_value()) << plugin.error().Message();
  const std::vector<std::string> five{"5"};
  ASSERT_TRUE(plugin->Call("bump", five).has_value());

  auto refused = plugin->Reload(AsBytes(greedy));
  ASSERT_FALSE(refused.has_value());
  EXPECT_EQ(refused.error().Code(), WT_ERR_PERMISSION_DENIED);
  EXPECT_NE(refused.error().Message().find("wasi:sockets"), std::string::npos)
      << refused.error().Message();

  // The handle is still valid and the old instance is still the one running.
  auto value = plugin->Call("get");
  ASSERT_TRUE(value.has_value()) << value.error().Message();
  EXPECT_EQ(value->value_or(""), "5");

  auto stats = plugin->Stats();
  ASSERT_TRUE(stats.has_value()) << stats.error().Message();
  EXPECT_EQ(stats->reloads, 0U) << "nothing was replaced";
}

TEST(CApi, PublishesTheStateHookNames) {
  // A world author reads these off the header rather than out of prose, so
  // they are part of the API.
  EXPECT_STREQ(wt_save_state_export(), "save-state");
  EXPECT_STREQ(wt_restore_state_export(), "restore-state");
}

TEST(CApi, ReloadFileReplacesTheCodeAndKeepsTheName) {
  const std::filesystem::path dir =
      std::filesystem::temp_directory_path() / "watoots_reload_test";
  std::filesystem::create_directories(dir);
  const std::filesystem::path replacement = dir / "counter-v2.wat";
  {
    std::ofstream out(replacement);
    out << kCounterV2;
  }

  const wt::Host host = BuildHost();
  const std::string first = kCounterV1;
  auto plugin = host.LoadBinary("counter", AsBytes(first));
  ASSERT_TRUE(plugin.has_value()) << plugin.error().Message();

  auto report = plugin->ReloadFile(replacement.string());
  ASSERT_TRUE(report.has_value()) << report.error().Message();
  EXPECT_TRUE(report->state_restored);
  EXPECT_EQ(plugin->Name(), "counter");

  auto value = plugin->Call("get");
  ASSERT_TRUE(value.has_value()) << value.error().Message();
  EXPECT_EQ(value->value_or(""), "1000");

  std::filesystem::remove_all(dir);
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
  // Marshalling is defined as the remainder, so the buckets always add up to
  // the wall time. That is the definition rather than a measurement, and this
  // pins it. There are four: `wave_nanos` joined them when the profiler was
  // found to be measuring only the call and not the text conversion around it,
  // and this assertion is what noticed the arithmetic had changed.
  EXPECT_EQ(profile->guest_nanos + profile->host_nanos +
                profile->marshalling_nanos + profile->wave_nanos,
            profile->wall_nanos);
  // And every call through this API pays that conversion, because
  // `wt_plugin_call` takes text and there is no other path.
  EXPECT_GT(profile->wave_nanos, 0U);

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

// ---------------------------------------------------------------------------
// The audit trail (ADR-0011)
// ---------------------------------------------------------------------------

TEST(CApi, AnAuditHookSeesADenialAndNamesTheImport) {
  std::vector<wt::AuditEvent> trail;

  wt::HostBuilder builder;
  ASSERT_TRUE(builder.ManifestFromString("").has_value());
  ASSERT_TRUE(builder
                  .AuditHook([&trail](const wt::AuditEvent& event) {
                    trail.push_back(event);
                  })
                  .has_value());
  auto host = builder.Build();
  ASSERT_TRUE(host.has_value()) << host.error().Message();

  const std::string wasm = kWantsNetwork;
  auto plugin = host->LoadBinary("net", AsBytes(wasm));
  ASSERT_FALSE(plugin.has_value());

  // The verdict, then the refusal it explains.
  ASSERT_EQ(trail.size(), 2U);
  const wt::AuditEvent& verdict = trail.front();
  EXPECT_EQ(verdict.kind, WT_AUDIT_IMPORT);
  EXPECT_EQ(verdict.verdict, WT_AUDIT_DENIED);
  EXPECT_EQ(verdict.import_name, "wasi:sockets/tcp@0.2.6");
  EXPECT_EQ(verdict.requirement, "network");

  // The event someone reads after an incident: not "a load was refused" but
  // which import refused it, and the key an operator would edit.
  const wt::AuditEvent& refusal = trail.back();
  EXPECT_EQ(refusal.kind, WT_AUDIT_LOAD_REFUSED);
  EXPECT_EQ(refusal.plugin, "net");
  EXPECT_EQ(refusal.import_name, "wasi:sockets/tcp@0.2.6");
  EXPECT_EQ(refusal.grant_key, "permissions.net");
  EXPECT_EQ(refusal.sha256.size(), 64U);
  EXPECT_NE(refusal.line.find("import=wasi:sockets/tcp@0.2.6"),
            std::string::npos)
      << refusal.line;
}

TEST(CApi, ASuppressedLogLineIsRecordedWithoutItsMessage) {
  std::vector<wt::AuditEvent> trail;

  wt::HostBuilder builder;
  ASSERT_TRUE(
      builder.ManifestFromString("[permissions]\nlogging = \"critical\"\n")
          .has_value());
  ASSERT_TRUE(builder
                  .AuditHook([&trail](const wt::AuditEvent& event) {
                    trail.push_back(event);
                  })
                  .has_value());
  ASSERT_TRUE(builder
                  .LogSink([](wt_log_level, std::string_view,
                              std::string_view) { FAIL(); })
                  .has_value());
  auto host = builder.Build();
  ASSERT_TRUE(host.has_value()) << host.error().Message();

  const std::string wasm = kLogsOnce;
  auto plugin = host->LoadBinary("talker", AsBytes(wasm));
  ASSERT_TRUE(plugin.has_value()) << plugin.error().Message();
  auto result = plugin->Call("run");
  ASSERT_TRUE(result.has_value()) << result.error().Message();

  const auto suppressed =
      std::ranges::find_if(trail, [](const wt::AuditEvent& event) {
        return event.kind == WT_AUDIT_LOG_SUPPRESSED;
      });
  ASSERT_NE(suppressed, trail.end());
  EXPECT_TRUE(suppressed->level_known);
  EXPECT_EQ(suppressed->level, WT_LOG_WARN);
  EXPECT_EQ(suppressed->ceiling_level, WT_LOG_CRITICAL);

  // The constraint the whole hook is built around. `kLogsOnce` logs the context
  // "boot" and the message "config is malformed"; an audit record has to be
  // safe to keep and to paste into an issue, so neither may be anywhere in it.
  for (const wt::AuditEvent& event : trail) {
    for (const std::string& field :
         {event.line, event.plugin, event.sha256, event.import_name,
          event.requirement, event.grant_key, event.reason, event.limit_key}) {
      EXPECT_EQ(field.find("config is malformed"), std::string::npos) << field;
      EXPECT_EQ(field.find("boot"), std::string::npos) << field;
    }
  }
}

TEST(CApi, AuditNamesAreStableAndMatchTheRenderedLine) {
  EXPECT_STREQ(wt_audit_kind_name(WT_AUDIT_LOG_SUPPRESSED), "log-suppressed");
  EXPECT_STREQ(wt_audit_verdict_name(WT_AUDIT_HOST_PROVIDED), "host-provided");
  EXPECT_STREQ(wt_ceiling_name(WT_CEILING_LOG_BYTES), "limits.log_bytes");

  std::vector<wt::AuditEvent> trail;
  wt::HostBuilder builder;
  ASSERT_TRUE(builder.ManifestFromString("").has_value());
  ASSERT_TRUE(builder
                  .AuditHook([&trail](const wt::AuditEvent& event) {
                    trail.push_back(event);
                  })
                  .has_value());
  auto host = builder.Build();
  ASSERT_TRUE(host.has_value()) << host.error().Message();

  const std::string wasm = kSelfContained;
  auto plugin = host->LoadBinary("answer", AsBytes(wasm));
  ASSERT_TRUE(plugin.has_value()) << plugin.error().Message();

  // Two events: the load, and the fact that nobody checked who wrote it. This
  // manifest lists no signature keys, so the second is not optional.
  ASSERT_EQ(trail.size(), 2U);
  const wt::AuditEvent& loaded = trail.front();
  EXPECT_EQ(loaded.kind, WT_AUDIT_LOADED);
  EXPECT_EQ(loaded.imports, 0U);
  // A log written from the structured fields and one written from the line have
  // to agree about what happened.
  EXPECT_EQ(loaded.line.rfind(wt_audit_kind_name(loaded.kind), 0), 0U)
      << loaded.line;

  const wt::AuditEvent& unverified = trail.back();
  EXPECT_EQ(unverified.kind, WT_AUDIT_LOADED_UNVERIFIED);
  EXPECT_STREQ(wt_audit_kind_name(WT_AUDIT_LOADED_UNVERIFIED),
               "loaded-unverified");
  // The rendered line has to carry the risk, not just the label: a C host
  // forwarding this to its own logger forwards the line.
  EXPECT_NE(unverified.line.find("risk=publisher-unchecked"), std::string::npos)
      << unverified.line;
}

TEST(CApi, ANullAuditHookIsRejected) {
  wt_error_t* error = nullptr;
  const wt_status status =
      wt_host_builder_audit_hook(nullptr, nullptr, nullptr, &error);
  EXPECT_EQ(status, WT_ERR_INVALID_ARGUMENT);
  wt_error_delete(error);
}

TEST(CApi, NoAuditHookMeansNoAuditTrailAndNoBehaviourChange) {
  // The claim `docs/SECURITY.md` makes out loud: an application that installs
  // no hook gets no audit trail. What it must not get is different behaviour.
  wt::HostBuilder builder;
  ASSERT_TRUE(builder.ManifestFromString("").has_value());
  auto host = builder.Build();
  ASSERT_TRUE(host.has_value()) << host.error().Message();

  const std::string wasm = kSelfContained;
  auto plugin = host->LoadBinary("answer", AsBytes(wasm));
  ASSERT_TRUE(plugin.has_value()) << plugin.error().Message();
  auto result = plugin->Call("answer");
  ASSERT_TRUE(result.has_value()) << result.error().Message();
  // value_or rather than value, for the reason given in
  // LoadsAndCallsAComponent: the fatal assert is not proof of engagement to
  // bugprone-unchecked-optional-access.
  const std::optional<std::string>& returned = *result;
  ASSERT_TRUE(returned.has_value());
  EXPECT_EQ(returned.value_or(""), "42");
}
