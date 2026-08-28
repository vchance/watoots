//! The manifest: what a plugin is allowed to touch, and how much of it.
//!
//! This is the piece the spec calls "the product". Everything else in the host
//! library exists to enforce what a manifest says.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

use crate::{Error, ErrorKind, Result};

/// Memory ceiling applied when the manifest does not set one.
///
/// Limits default to *something* rather than to unlimited: an absent `[limits]`
/// table should not mean "this plugin may exhaust the host".
pub const DEFAULT_MEMORY_BYTES: u64 = 64 * 1024 * 1024;

/// A parsed manifest.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Manifest {
    /// What the plugin may reach.
    pub permissions: Permissions,
    /// How much it may consume.
    pub limits: Limits,
}

/// Capability grants. Everything defaults to denied.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Permissions {
    /// Filesystem grants.
    pub fs: FsGrants,
    /// Whether the plugin may see the socket interfaces, and which hosts it
    /// may reach.
    ///
    /// Three states, because importing an interface and being able to use it
    /// are different questions:
    ///
    /// - absent — `wasi:sockets` and `wasi:http` are denied outright, and a
    ///   component importing them fails to load.
    /// - `net = []` — the interfaces are available and *every* connection is
    ///   refused. This is what a CPython or JS guest needs: their runtimes link
    ///   the socket interfaces whether or not the plugin opens one.
    /// - `net = ["example.com"]` — an allowlist, which nothing enforces yet, so
    ///   building a host with one is refused rather than silently over-granted.
    pub net: Option<Vec<String>>,
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
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            memory: DEFAULT_MEMORY_BYTES,
            fuel: None,
            timeout: None,
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
            .chain(self.permissions.net.iter_mut().flatten())
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
        assert!(manifest.permissions.net.is_none());
        assert!(manifest.permissions.env.is_none());
        assert_eq!(manifest.permissions.clocks, Clocks::None);
        assert!(!manifest.permissions.random);
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
            net      = []
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
    fn an_empty_net_list_grants_the_interface_but_no_hosts() {
        let denied = Manifest::parse("").unwrap();
        assert!(denied.permissions.net.is_none());

        let interface_only = Manifest::parse("[permissions]\nnet = []\n").unwrap();
        assert_eq!(interface_only.permissions.net, Some(Vec::new()));
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
