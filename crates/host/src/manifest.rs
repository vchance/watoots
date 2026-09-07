//! The manifest: what a plugin is allowed to touch, and how much of it.
//!
//! This is the piece the spec calls "the product". Everything else in the host
//! library exists to enforce what a manifest says.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

use crate::{Error, ErrorKind, Result};

/// Memory ceiling applied when the manifest does not set one.
///
/// Limits default to *something* rather than to unlimited: an absent `[limits]`
/// table should not mean "this plugin may exhaust the host".
pub const DEFAULT_MEMORY_BYTES: u64 = 64 * 1024 * 1024;

/// Log bytes a plugin may push across the boundary in one call, by default.
///
/// Same reasoning as [`DEFAULT_MEMORY_BYTES`]: a ceiling nobody stated is still
/// a ceiling. Fuel bounds how many times a plugin loops; it does not bound how
/// much text each iteration hands to the host's logging pipeline, and lifting a
/// guest string into a host `String` is work the host pays for outside the fuel
/// budget.
pub const DEFAULT_LOG_BYTES: u64 = 64 * 1024;

/// Log messages a plugin may emit in one call, by default.
pub const DEFAULT_LOG_MESSAGES: u64 = 1024;

/// What a guest may hand the host in one crossing, by default.
///
/// Wasmtime's own default, kept rather than raised. It is a DoS mitigation, and
/// a host library that quietly loosened someone else's security default would
/// be the wrong kind of convenient — a manifest that needs more should say so.
pub const DEFAULT_TRANSFER_BYTES: u64 = 128 * 1024 * 1024;

/// A parsed manifest.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Manifest {
    /// What the plugin may reach.
    pub permissions: Permissions,
    /// How much it may consume.
    pub limits: Limits,
    /// How reproducible its execution has to be.
    pub determinism: Determinism,
    /// Who is allowed to have signed it.
    pub signature: SignaturePolicy,
}

/// Which publishers a plugin loaded under this manifest may come from.
///
/// **The one part of a manifest where absence does not deny.** Everywhere else
/// an omitted key refuses the capability; here an omitted `[signature]` section
/// means signatures are not checked, because the alternative is that upgrading
/// watoots stops every existing plugin from loading. `docs/MANIFEST.md` calls
/// the asymmetry out rather than leaving a reader to assume the usual rule.
///
/// Once `keys` is non-empty the usual posture returns: a plugin with no
/// signature, or one no listed key verifies, does not load. There is no warn
/// mode and no trust-on-first-use — see ADR-0014.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SignaturePolicy {
    /// PEM `PUBLIC KEY` blocks, any one of which may have signed the plugin.
    ///
    /// More than one because that is what a rotation looks like: the old key
    /// and the new one are both trusted for as long as it takes to re-sign.
    ///
    /// The key is P-256, which is `cosign`'s default and what
    /// `cosign public-key` or `openssl ec -pubout` writes.
    pub keys: Vec<String>,
}

impl SignaturePolicy {
    /// Whether a plugin has to be signed to load under this manifest.
    #[must_use]
    pub fn is_required(&self) -> bool {
        !self.keys.is_empty()
    }
}

/// Knobs that make two runs of the same plugin agree.
///
/// These live in the manifest rather than on the builder because the manifest
/// travels inside a recorded trace. Replay rebuilds its host from that header,
/// so record and replay cannot end up configured differently — which would make
/// a divergence report a statement about the engine rather than the plugin.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Determinism {
    /// Canonicalise NaNs, make relaxed-SIMD deterministic, and pin the clocks
    /// and random generators to fixed values.
    ///
    /// On by default. A plugin host's characteristic failure is "it behaved
    /// differently on a user's machine and I cannot reproduce it", and this is
    /// what prevents it. NaN canonicalisation costs something on float-heavy
    /// guests; turn it off there and accept that their traces are less portable.
    pub enabled: bool,
    /// What the wall clock reads, in seconds since the Unix epoch.
    pub epoch_seconds: u64,
    /// How far the monotonic clock advances per read, in nanoseconds. It must
    /// move or a guest that polls will spin, and it must move predictably or
    /// the recording is not reproducible.
    pub monotonic_step_nanos: u64,
    /// Seed for the random generators.
    pub seed: String,
}

impl Default for Determinism {
    fn default() -> Self {
        Self {
            enabled: true,
            epoch_seconds: 0,
            monotonic_step_nanos: 1_000_000,
            seed: "watoots".to_string(),
        }
    }
}

/// Capability grants. Everything defaults to denied.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Permissions {
    /// Filesystem grants.
    pub fs: FsGrants,
    /// Whether the plugin may *import* the socket interfaces.
    ///
    /// Two states, because that is how many watoots can tell apart. There is
    /// no host allowlist: see [`NetGrant`] and ADR-0012.
    pub net: NetGrant,
    /// Environment variables handed to the guest, by name and value. The guest
    /// sees exactly this map and nothing inherited from the host process.
    ///
    /// `None` (the key absent) denies `wasi:cli/environment` outright.
    /// `Some` — including `env = {}` — grants it; an empty map simply means the
    /// guest may look and will find nothing. The distinction matters because
    /// "may read an empty environment" and "may not read" are different grants.
    pub env: Option<BTreeMap<String, String>>,
    /// Which clocks the plugin may read.
    pub clocks: Clocks,
    /// Whether `wasi:random` is available.
    pub random: bool,
    /// The least severe level `wasi:logging` will deliver, or `None` to deny
    /// the interface outright.
    ///
    /// `logging = "warn"` admits `warn`, `error` and `critical` and drops
    /// everything below; `logging = "trace"` admits all six. Absence denies, so
    /// a component importing `wasi:logging` fails to load — the same rule as
    /// every other permission.
    ///
    /// Deliberately *not* tri-state the way `env` is, and not two-state the way
    /// `net` is. Those have their extra state because a runtime links their
    /// interfaces whether or not the plugin uses them, so "may import, may not
    /// reach anything" is a situation that really occurs. Nothing links
    /// `wasi:logging` behind an author's
    /// back — it is not part of WASI 0.2 — and "granted but restricted" is
    /// already what a level says, so a fourth spelling would mean nothing new.
    pub logging: Option<LogLevel>,
}

/// Whether a plugin may import `wasi:sockets` and `wasi:http`.
///
/// Not an allowlist, and deliberately so. watoots sits at the component
/// boundary, where `wasmtime-wasi`'s `socket_addr_check` is handed a resolved
/// [`SocketAddr`](std::net::SocketAddr) and never the name that produced it —
/// so a rule about `api.example.com` has nothing here to match on. A manifest
/// key that looks like it restricts something and does not is worse than no key
/// at all, so there is no key. ADR-0012 has the full argument, and
/// `docs/SECURITY.md` says where hostname policy does belong.
///
/// This does not mean a granted plugin reaches the network. `wasmtime-wasi`
/// refuses every connection unless the embedder installs a check, and watoots
/// installs none — so [`Linked`](Self::Linked) means the imports resolve and
/// every connection still fails.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum NetGrant {
    /// `wasi:sockets` and `wasi:http` are denied outright: a component
    /// importing either fails to load. The default, and what an absent `net`
    /// key means.
    #[default]
    Deny,
    /// The interfaces resolve and every connection is refused.
    ///
    /// This exists for the guests that leave no choice. CPython links
    /// `wasi:sockets` whether or not the plugin opens a socket, and a
    /// JavaScript runtime does the same, so denying the import means the plugin
    /// cannot load at all. Granting it costs nothing while no check is
    /// installed: there is no reachable host either way.
    Linked,
}

impl NetGrant {
    /// Whether the socket interfaces may be imported at all.
    #[must_use]
    pub fn is_granted(self) -> bool {
        self != Self::Deny
    }

    /// The spelling a manifest uses for this grant.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deny => "deny",
            Self::Linked => "linked",
        }
    }
}

impl<'de> Deserialize<'de> for NetGrant {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        struct GrantVisitor;

        impl<'de> serde::de::Visitor<'de> for GrantVisitor {
            type Value = NetGrant;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(r#"`"deny"` or `"linked"`"#)
            }

            fn visit_str<E: serde::de::Error>(
                self,
                value: &str,
            ) -> std::result::Result<NetGrant, E> {
                match value {
                    "deny" => Ok(NetGrant::Deny),
                    "linked" => Ok(NetGrant::Linked),
                    other => Err(E::custom(format!(
                        r#"unknown net grant {other:?}; expected "deny" or "linked""#
                    ))),
                }
            }

            // Every manifest written before 0.3 spells this as a list, so the
            // error has to say what to write instead. serde's own message for
            // a sequence where a string was wanted would send the reader to
            // the type rather than to the change.
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                _: A,
            ) -> std::result::Result<NetGrant, A::Error> {
                Err(serde::de::Error::custom(
                    "permissions.net is no longer a list of hosts: nothing at this \
                     layer could enforce one, so the allowlist was removed rather \
                     than left looking enforced. Write `net = \"linked\"` for what \
                     `net = []` meant - the socket interfaces resolve and every \
                     connection is refused - and see docs/SECURITY.md for where \
                     hostname policy belongs.",
                ))
            }
        }

        deserializer.deserialize_str(GrantVisitor)
    }
}

/// A `wasi:logging` severity.
///
/// The six cases, their spellings and their order are `wasi:logging`'s own, so
/// the discriminant a guest passes for `level` is this enum's index and the
/// derived ordering is severity order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LogLevel {
    /// Values of variables and the flow of control within a program.
    Trace,
    /// Of interest to someone debugging the program.
    Debug,
    /// Of interest to someone monitoring the program.
    Info,
    /// A hazardous situation.
    Warn,
    /// A serious error.
    Error,
    /// A fatal error.
    Critical,
}

impl LogLevel {
    /// Every level, least severe first.
    pub const ALL: [Self; 6] = [
        Self::Trace,
        Self::Debug,
        Self::Info,
        Self::Warn,
        Self::Error,
        Self::Critical,
    ];

    /// The spelling `wasi:logging` uses for this case.
    #[must_use]
    pub fn as_wit_name(self) -> &'static str {
        match self {
            Self::Trace => "trace",
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
            Self::Critical => "critical",
        }
    }

    /// The level a `wasi:logging` enum case names, or `None` for a spelling the
    /// proposal does not define.
    ///
    /// Unknown is `None` rather than a fallback level: guessing would let a
    /// future case slip past a manifest's ceiling.
    #[must_use]
    pub fn from_wit_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|level| level.as_wit_name() == name)
    }
}

/// Read and write grants, as host paths.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct FsGrants {
    /// Directories preopened read-only.
    pub read: Vec<String>,
    /// Directories preopened read-write.
    pub write: Vec<String>,
}

impl FsGrants {
    /// Whether any filesystem access at all is granted.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.read.is_empty() && self.write.is_empty()
    }
}

/// Which clocks a plugin may read.
///
/// Wall-clock access is separate from monotonic because it is the one that
/// makes a recorded trace non-reproducible.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Clocks {
    /// No clock at all.
    #[default]
    None,
    /// Monotonic only: durations, but no idea what time it is.
    Monotonic,
    /// Monotonic and wall clock.
    Wall,
}

impl Clocks {
    /// Whether the monotonic clock is readable.
    #[must_use]
    pub fn allows_monotonic(self) -> bool {
        matches!(self, Self::Monotonic | Self::Wall)
    }

    /// Whether the wall clock is readable.
    #[must_use]
    pub fn allows_wall(self) -> bool {
        matches!(self, Self::Wall)
    }
}

/// Resource ceilings.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Limits {
    /// Linear-memory ceiling in bytes. Accepts `"64MiB"` or a plain integer.
    #[serde(deserialize_with = "deserialize_bytes")]
    pub memory: u64,
    /// Fuel granted per call. `None` leaves fuel metering off entirely.
    pub fuel: Option<u64>,
    /// Wall-clock deadline per call, enforced with epoch interruption.
    /// Accepts `"200ms"`. `None` leaves epoch interruption off.
    #[serde(with = "humantime_serde")]
    pub timeout: Option<Duration>,
    /// Log text a plugin may push across the boundary per call, counting the
    /// context and the message together. Accepts `"64KiB"` or a plain integer.
    #[serde(deserialize_with = "deserialize_bytes")]
    pub log_bytes: u64,
    /// Log messages a plugin may emit per call.
    ///
    /// Separate from `log_bytes` because the two abuses are different: one
    /// enormous message and a million empty ones cost the host in different
    /// places, and a cap that stops only one of them stops neither.
    pub log_messages: u64,
    /// How much the guest may hand the host in a single crossing.
    ///
    /// Wasmtime's own DoS mitigation, which watoots did not previously expose:
    /// it bounds what the *host* allocates on the guest's behalf when lifting
    /// arguments and return values, so a plugin cannot ask for an unbounded
    /// allocation on this side of the boundary. Per crossing, not per call, and
    /// reset for each one. Only guest-to-host is metered — data going the other
    /// way is already resident here.
    ///
    /// **The unit is not payload bytes on the dynamic path.** Lifting into
    /// `Val` costs `size_of::<Val>()` per *element*, so a `list<u8>` spends 48
    /// per byte: the default 128 MiB admits about 2.79 MB of `list<u8>`, which
    /// is the wall a plugin returning an image hits. Divide by 48 to size this
    /// for a byte payload; a typed `bindgen!` host would not pay it, because
    /// `list<u8>` lifts into `Vec<u8>` there.
    ///
    /// Accepts `"128MiB"` or a plain integer.
    #[serde(deserialize_with = "deserialize_bytes")]
    pub transfer: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            memory: DEFAULT_MEMORY_BYTES,
            fuel: None,
            timeout: None,
            log_bytes: DEFAULT_LOG_BYTES,
            log_messages: DEFAULT_LOG_MESSAGES,
            transfer: DEFAULT_TRANSFER_BYTES,
        }
    }
}

/// Accept either `memory = "64MiB"` or `memory = 67108864`.
fn deserialize_bytes<'de, D>(deserializer: D) -> std::result::Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Repr {
        Text(String),
        Bytes(u64),
    }

    match Repr::deserialize(deserializer)? {
        Repr::Bytes(bytes) => Ok(bytes),
        Repr::Text(text) => text
            .parse::<bytesize::ByteSize>()
            .map(|size| size.as_u64())
            .map_err(serde::de::Error::custom),
    }
}

impl Manifest {
    /// Parse a manifest from TOML.
    ///
    /// Unknown keys are an error. A typo in a permission name must not read as
    /// "not granted" — that would fail open at review time, where someone sees
    /// `fs.raed` in a diff and assumes it did something.
    pub fn parse(toml_text: &str) -> Result<Self> {
        toml::from_str(toml_text).map_err(|err| Error::new(ErrorKind::Manifest, err.to_string()))
    }

    /// Read and parse a manifest from disk.
    pub fn from_file(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path).map_err(|err| {
            Error::new(
                ErrorKind::Manifest,
                format!("cannot read manifest {}: {err}", path.display()),
            )
        })?;
        Self::parse(&text)
    }

    /// Expand `${name}` references in filesystem paths and environment values.
    ///
    /// An unknown variable is an error rather than an empty string: silently
    /// expanding `${workspace}/**` to `/**` would widen a grant instead of
    /// narrowing it.
    pub fn substitute(&mut self, vars: &BTreeMap<String, String>) -> Result<()> {
        for path in self
            .permissions
            .fs
            .read
            .iter_mut()
            .chain(self.permissions.fs.write.iter_mut())
        {
            *path = expand(path, vars)?;
        }
        if let Some(env) = self.permissions.env.as_mut() {
            for value in env.values_mut() {
                *value = expand(value, vars)?;
            }
        }
        Ok(())
    }
}

fn expand(input: &str, vars: &BTreeMap<String, String>) -> Result<String> {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;

    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after.find('}').ok_or_else(|| {
            Error::new(
                ErrorKind::Manifest,
                format!("unterminated `${{` in {input:?}"),
            )
        })?;
        let name = &after[..end];
        let value = vars.get(name).ok_or_else(|| {
            let known: Vec<&str> = vars.keys().map(String::as_str).collect();
            Error::new(
                ErrorKind::Manifest,
                format!(
                    "unknown variable `${{{name}}}` in {input:?}; defined: {}",
                    if known.is_empty() {
                        "(none)".to_string()
                    } else {
                        known.join(", ")
                    }
                ),
            )
        })?;
        out.push_str(value);
        rest = &after[end + 1..];
    }

    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn empty_manifest_denies_everything() {
        let manifest = Manifest::parse("").unwrap();
        assert!(manifest.permissions.fs.is_empty());
        assert_eq!(manifest.permissions.net, NetGrant::Deny);
        assert!(manifest.permissions.env.is_none());
        assert_eq!(manifest.permissions.clocks, Clocks::None);
        assert!(!manifest.permissions.random);
        assert_eq!(manifest.permissions.logging, None);
    }

    #[test]
    fn a_logging_grant_names_the_least_severe_level_delivered() {
        let manifest = Manifest::parse("[permissions]\nlogging = \"warn\"\n").unwrap();
        assert_eq!(manifest.permissions.logging, Some(LogLevel::Warn));

        let err = Manifest::parse("[permissions]\nlogging = \"chatty\"\n").unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Manifest);
    }

    #[test]
    fn log_levels_order_by_severity_and_round_trip_their_wit_spelling() {
        assert!(LogLevel::Trace < LogLevel::Warn);
        assert!(LogLevel::Warn < LogLevel::Critical);
        for level in LogLevel::ALL {
            assert_eq!(LogLevel::from_wit_name(level.as_wit_name()), Some(level));
        }
        assert_eq!(LogLevel::from_wit_name("verbose"), None);
    }

    #[test]
    fn empty_manifest_still_caps_log_volume() {
        let manifest = Manifest::parse("").unwrap();
        assert_eq!(manifest.limits.log_bytes, DEFAULT_LOG_BYTES);
        assert_eq!(manifest.limits.log_messages, DEFAULT_LOG_MESSAGES);

        let tightened =
            Manifest::parse("[limits]\nlog_bytes = \"1KiB\"\nlog_messages = 4\n").unwrap();
        assert_eq!(tightened.limits.log_bytes, 1024);
        assert_eq!(tightened.limits.log_messages, 4);
    }

    #[test]
    fn determinism_is_on_unless_switched_off() {
        assert!(Manifest::parse("").unwrap().determinism.enabled);
        let off = Manifest::parse("[determinism]\nenabled = false\n").unwrap();
        assert!(!off.determinism.enabled);
    }

    #[test]
    fn empty_manifest_still_caps_memory() {
        let manifest = Manifest::parse("").unwrap();
        assert_eq!(manifest.limits.memory, DEFAULT_MEMORY_BYTES);
        assert_eq!(manifest.limits.fuel, None);
        assert_eq!(manifest.limits.timeout, None);
    }

    #[test]
    fn parses_the_manifest_from_the_spec() {
        let manifest = Manifest::parse(
            r#"
            [permissions]
            fs.read  = ["${plugin_dir}", "${workspace}/**/*.md"]
            fs.write = ["${plugin_dir}/cache"]
            net      = "linked"
            clocks   = "monotonic"
            random   = true

            [limits]
            memory   = "64MiB"
            fuel     = 50_000_000
            timeout  = "200ms"
            "#,
        )
        .unwrap();

        assert_eq!(manifest.permissions.fs.read.len(), 2);
        assert_eq!(manifest.permissions.fs.write, ["${plugin_dir}/cache"]);
        assert_eq!(manifest.permissions.clocks, Clocks::Monotonic);
        assert!(manifest.permissions.random);
        assert_eq!(manifest.limits.memory, 64 * 1024 * 1024);
        assert_eq!(manifest.limits.fuel, Some(50_000_000));
        assert_eq!(manifest.limits.timeout, Some(Duration::from_millis(200)));
    }

    #[test]
    fn memory_accepts_a_plain_integer() {
        let manifest = Manifest::parse("[limits]\nmemory = 1024\n").unwrap();
        assert_eq!(manifest.limits.memory, 1024);
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let err = Manifest::parse("[permissions]\nfs.raed = [\"/tmp\"]\n").unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Manifest);
        assert!(err.message().contains("raed"), "{}", err.message());
    }

    #[test]
    fn unknown_clock_setting_is_rejected() {
        let err = Manifest::parse("[permissions]\nclocks = \"sundial\"\n").unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Manifest);
    }

    #[test]
    fn substitution_expands_known_variables() {
        let mut manifest = Manifest::parse(
            r#"
            [permissions]
            fs.read = ["${plugin_dir}", "${workspace}/docs"]
            env = { HOME = "${plugin_dir}/home" }
            "#,
        )
        .unwrap();

        manifest
            .substitute(&vars(&[("plugin_dir", "/opt/p"), ("workspace", "/w")]))
            .unwrap();

        assert_eq!(manifest.permissions.fs.read, ["/opt/p", "/w/docs"]);
        assert_eq!(
            manifest.permissions.env.as_ref().unwrap()["HOME"],
            "/opt/p/home"
        );
    }

    #[test]
    fn substitution_rejects_an_unknown_variable() {
        let mut manifest = Manifest::parse("[permissions]\nfs.read = [\"${nope}/x\"]\n").unwrap();
        let err = manifest
            .substitute(&vars(&[("plugin_dir", "/opt/p")]))
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Manifest);
        assert!(err.message().contains("nope"), "{}", err.message());
        assert!(err.message().contains("plugin_dir"), "{}", err.message());
    }

    #[test]
    fn substitution_rejects_an_unterminated_reference() {
        let mut manifest = Manifest::parse("[permissions]\nfs.read = [\"${oops\"]\n").unwrap();
        let err = manifest.substitute(&vars(&[])).unwrap_err();
        assert!(err.message().contains("unterminated"), "{}", err.message());
    }

    #[test]
    fn net_grants_the_import_and_never_a_host() {
        let denied = Manifest::parse("").unwrap();
        assert_eq!(denied.permissions.net, NetGrant::Deny);
        assert!(!denied.permissions.net.is_granted());

        let linked = Manifest::parse("[permissions]\nnet = \"linked\"\n").unwrap();
        assert_eq!(linked.permissions.net, NetGrant::Linked);
        assert!(linked.permissions.net.is_granted());

        let explicit = Manifest::parse("[permissions]\nnet = \"deny\"\n").unwrap();
        assert_eq!(explicit.permissions.net, NetGrant::Deny);
    }

    #[test]
    fn a_net_list_is_refused_with_the_replacement_spelling() {
        // Every manifest written before 0.3 spells this as a list. The error is
        // the only place such a reader finds out what to write instead, so it
        // has to carry the new spelling rather than serde's type mismatch.
        for source in [
            "[permissions]\nnet = []\n",
            "[permissions]\nnet = [\"example.com\"]\n",
        ] {
            let err = Manifest::parse(source).unwrap_err();
            let message = err.message();
            assert!(message.contains("no longer a list"), "{message}");
            assert!(message.contains(r#"net = "linked""#), "{message}");
        }
    }

    #[test]
    fn an_unknown_net_grant_names_the_ones_that_exist() {
        let err = Manifest::parse("[permissions]\nnet = \"allow\"\n").unwrap_err();
        assert!(
            err.message().contains(r#""deny" or "linked""#),
            "{}",
            err.message()
        );
    }

    #[test]
    fn an_empty_env_table_is_a_grant_not_an_absence() {
        let denied = Manifest::parse("").unwrap();
        assert!(denied.permissions.env.is_none());

        let granted = Manifest::parse("[permissions]\nenv = {}\n").unwrap();
        assert_eq!(granted.permissions.env, Some(BTreeMap::new()));
    }

    #[test]
    fn clock_ordering_is_a_ladder() {
        assert!(!Clocks::None.allows_monotonic());
        assert!(!Clocks::None.allows_wall());
        assert!(Clocks::Monotonic.allows_monotonic());
        assert!(!Clocks::Monotonic.allows_wall());
        assert!(Clocks::Wall.allows_monotonic());
        assert!(Clocks::Wall.allows_wall());
    }
}
