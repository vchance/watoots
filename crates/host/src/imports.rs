//! The import-intersection check.
//!
//! At load time we enumerate what a component actually imports and intersect it
//! with what the manifest grants. Anything not covered is a *load* error, not a
//! runtime trap: the point of a manifest is that you learn a plugin wants the
//! network when you install it, not when it first tries to use it.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use crate::manifest::Permissions;

/// A parsed WIT interface name: `namespace:package/interface@version`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceRef {
    /// e.g. `wasi`
    pub namespace: String,
    /// e.g. `filesystem`
    pub package: String,
    /// e.g. `types`
    pub interface: String,
    /// e.g. `0.2.6`, when the import carries one.
    pub version: Option<String>,
}

impl InterfaceRef {
    /// Parse an import name, or `None` if it is not an interface reference.
    ///
    /// Bare function imports and core-module imports do not have this shape;
    /// callers treat those as unrecognized rather than guessing.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let (package_path, rest) = raw.split_once('/')?;
        let (namespace, package) = package_path.split_once(':')?;
        let (interface, version) = match rest.split_once('@') {
            Some((interface, version)) => (interface, Some(version.to_string())),
            None => (rest, None),
        };
        if namespace.is_empty() || package.is_empty() || interface.is_empty() {
            return None;
        }
        Some(Self {
            namespace: namespace.to_string(),
            package: package.to_string(),
            interface: interface.to_string(),
            version,
        })
    }

    /// The name without its version, which is how grants are matched.
    ///
    /// Matching ignores the version deliberately: granting `wasi:filesystem`
    /// should not have to be re-stated when a guest is rebuilt against
    /// WASI 0.2.7, and the version is still reported for the audit trail.
    #[must_use]
    pub fn unversioned(&self) -> String {
        format!("{}:{}/{}", self.namespace, self.package, self.interface)
    }
}

/// One import as the component declares it.
#[derive(Debug, Clone, Copy)]
pub struct ComponentImport<'a> {
    /// The import name, e.g. `wasi:filesystem/types@0.2.12`.
    pub name: &'a str,
    /// Whether the imported instance exposes any callable function.
    ///
    /// WIT packages routinely import a sibling interface purely for its type
    /// definitions — `watoots:example/log` using `severity` from
    /// `watoots:example/types` pulls the whole types interface into the import
    /// list. There is nothing to call there, so there is nothing to grant.
    pub has_functions: bool,
}

/// What a given import needs in order to be allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Requirement {
    /// Plumbing every WASI 0.2 component pulls in, which conveys no capability
    /// on its own: streams, poll, error types, exit, stdio handles.
    Ambient,
    /// An interface with no callable functions, imported only for its types.
    TypesOnly,
    /// `wasi:filesystem` — needs some `fs.read` or `fs.write` grant.
    Filesystem,
    /// `wasi:sockets` — needs a non-empty `net` allowlist.
    Network,
    /// `wasi:clocks/monotonic-clock`.
    MonotonicClock,
    /// `wasi:clocks/wall-clock`.
    WallClock,
    /// `wasi:random`.
    Random,
    /// `wasi:cli/environment`.
    Environment,
    /// An interface the embedding application declared it serves.
    HostProvided,
    /// Something we cannot account for, which is therefore denied.
    Unrecognized,
}

impl Requirement {
    /// The manifest key an operator would edit to grant this.
    #[must_use]
    pub fn grant_key(self) -> &'static str {
        match self {
            Self::Ambient => "(none needed)",
            Self::TypesOnly => "(types only, nothing callable)",
            Self::Filesystem => "permissions.fs.read / permissions.fs.write",
            Self::Network => "permissions.net",
            Self::MonotonicClock => "permissions.clocks = \"monotonic\"",
            Self::WallClock => "permissions.clocks = \"wall\"",
            Self::Random => "permissions.random = true",
            Self::Environment => "permissions.env",
            Self::HostProvided => "(served by the host application)",
            Self::Unrecognized => "(cannot be granted)",
        }
    }
}

/// Classify one import against the WASI interfaces we know about.
#[must_use]
pub fn classify(import: ComponentImport<'_>, host_provided: &BTreeSet<String>) -> Requirement {
    let Some(iface) = InterfaceRef::parse(import.name) else {
        return Requirement::Unrecognized;
    };

    // Checked before anything else: an instance with no functions cannot be
    // called, so no grant could be about it either way.
    if !import.has_functions {
        return Requirement::TypesOnly;
    }

    if host_provided.contains(&iface.unversioned()) {
        return Requirement::HostProvided;
    }

    if iface.namespace != "wasi" {
        return Requirement::Unrecognized;
    }

    match (iface.package.as_str(), iface.interface.as_str()) {
        ("io", _) => Requirement::Ambient,
        ("cli", "environment") => Requirement::Environment,
        ("cli", name)
            if name == "exit"
                || name.starts_with("stdin")
                || name.starts_with("stdout")
                || name.starts_with("stderr")
                || name.starts_with("terminal") =>
        {
            Requirement::Ambient
        }
        ("filesystem", _) => Requirement::Filesystem,
        ("sockets", _) => Requirement::Network,
        ("clocks", "wall-clock") => Requirement::WallClock,
        ("clocks", _) => Requirement::MonotonicClock,
        ("random", _) => Requirement::Random,
        _ => Requirement::Unrecognized,
    }
}

/// One import, what it needs, and whether the manifest covers it.
#[derive(Debug, Clone)]
pub struct ImportDecision {
    /// The import name exactly as the component declared it.
    pub import: String,
    /// What it needs.
    pub requirement: Requirement,
    /// Whether the manifest covers it.
    pub granted: bool,
}

/// The full picture for one component: every import, classified and decided.
///
/// This is what v0.2's `watoots inspect` renders, and it is produced whether or
/// not the load succeeds, so a denial can explain itself.
#[derive(Debug, Clone)]
pub struct GrantReport {
    /// One entry per import, in the order the component declares them.
    pub decisions: Vec<ImportDecision>,
}

impl GrantReport {
    /// Imports the manifest does not cover.
    pub fn denied(&self) -> impl Iterator<Item = &ImportDecision> {
        self.decisions.iter().filter(|decision| !decision.granted)
    }

    /// Whether every import is covered.
    #[must_use]
    pub fn is_satisfied(&self) -> bool {
        self.decisions.iter().all(|decision| decision.granted)
    }

    /// A human-readable grant list.
    #[must_use]
    pub fn describe(&self) -> String {
        let mut out = String::new();
        for decision in &self.decisions {
            let mark = if decision.granted { "ok " } else { "DENY" };
            let _ = writeln!(
                out,
                "  {mark} {}  [{}]",
                decision.import,
                decision.requirement.grant_key()
            );
        }
        out
    }
}

/// Intersect a component's imports with what the manifest grants.
pub fn check<'a>(
    imports: impl IntoIterator<Item = ComponentImport<'a>>,
    permissions: &Permissions,
    host_provided: &BTreeSet<String>,
) -> GrantReport {
    let decisions = imports
        .into_iter()
        .map(|import| {
            let requirement = classify(import, host_provided);
            ImportDecision {
                import: import.name.to_string(),
                requirement,
                granted: is_granted(requirement, permissions),
            }
        })
        .collect();

    GrantReport { decisions }
}

fn is_granted(requirement: Requirement, permissions: &Permissions) -> bool {
    match requirement {
        Requirement::Ambient | Requirement::TypesOnly | Requirement::HostProvided => true,
        // `wasi:filesystem` covers reading and writing in one interface, so the
        // manifest cannot separate them here; the read/write split is enforced
        // by what each directory is preopened as.
        Requirement::Filesystem => !permissions.fs.is_empty(),
        Requirement::Network => !permissions.net.is_empty(),
        Requirement::MonotonicClock => permissions.clocks.allows_monotonic(),
        Requirement::WallClock => permissions.clocks.allows_wall(),
        Requirement::Random => permissions.random,
        // `env = {}` is a real grant meaning "an empty environment": the guest
        // may read it and will find nothing. Absent means it may not read at
        // all, which is why this is an Option and not an emptiness check.
        Requirement::Environment => permissions.env.is_some(),
        Requirement::Unrecognized => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Manifest;

    fn provided(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|n| (*n).to_string()).collect()
    }

    /// An import that has something callable in it.
    fn callable(name: &str) -> ComponentImport<'_> {
        ComponentImport {
            name,
            has_functions: true,
        }
    }

    #[test]
    fn parses_a_versioned_interface_name() {
        let iface = InterfaceRef::parse("wasi:filesystem/types@0.2.6").unwrap();
        assert_eq!(iface.namespace, "wasi");
        assert_eq!(iface.package, "filesystem");
        assert_eq!(iface.interface, "types");
        assert_eq!(iface.version.as_deref(), Some("0.2.6"));
        assert_eq!(iface.unversioned(), "wasi:filesystem/types");
    }

    #[test]
    fn parses_an_unversioned_interface_name() {
        let iface = InterfaceRef::parse("watoots:example/log").unwrap();
        assert_eq!(iface.version, None);
        assert_eq!(iface.unversioned(), "watoots:example/log");
    }

    #[test]
    fn rejects_names_that_are_not_interfaces() {
        assert!(InterfaceRef::parse("some-bare-func").is_none());
        assert!(InterfaceRef::parse("no-colon/iface").is_none());
        assert!(InterfaceRef::parse("ns:pkg").is_none());
        assert!(InterfaceRef::parse(":pkg/iface").is_none());
    }

    #[test]
    fn classifies_wasi_interfaces() {
        let none = provided(&[]);
        let cases = [
            ("wasi:io/streams@0.2.6", Requirement::Ambient),
            ("wasi:io/poll@0.2.6", Requirement::Ambient),
            ("wasi:cli/stdout@0.2.6", Requirement::Ambient),
            ("wasi:cli/exit@0.2.6", Requirement::Ambient),
            ("wasi:cli/environment@0.2.6", Requirement::Environment),
            ("wasi:filesystem/types@0.2.6", Requirement::Filesystem),
            ("wasi:filesystem/preopens@0.2.6", Requirement::Filesystem),
            ("wasi:sockets/tcp@0.2.6", Requirement::Network),
            ("wasi:clocks/wall-clock@0.2.6", Requirement::WallClock),
            (
                "wasi:clocks/monotonic-clock@0.2.6",
                Requirement::MonotonicClock,
            ),
            ("wasi:random/random@0.2.6", Requirement::Random),
        ];
        for (import, expected) in cases {
            assert_eq!(classify(callable(import), &none), expected, "{import}");
        }
    }

    #[test]
    fn unknown_wasi_package_is_not_silently_allowed() {
        // A future WASI package we have not classified must deny, not pass.
        assert_eq!(
            classify(callable("wasi:keyvalue/store@0.2.0"), &provided(&[])),
            Requirement::Unrecognized
        );
    }

    #[test]
    fn host_provided_interfaces_are_recognized_by_unversioned_name() {
        let host = provided(&["watoots:example/log"]);
        assert_eq!(
            classify(callable("watoots:example/log@0.1.0"), &host),
            Requirement::HostProvided
        );
        assert_eq!(
            classify(callable("watoots:example/other@0.1.0"), &host),
            Requirement::Unrecognized
        );
    }

    #[test]
    fn a_host_grant_can_shadow_a_wasi_interface() {
        // Virtualizing wasi:filesystem is a legitimate host trick; if the app
        // says it serves that interface, it is the app's call, not a grant.
        let host = provided(&["wasi:filesystem/types"]);
        assert_eq!(
            classify(callable("wasi:filesystem/types@0.2.6"), &host),
            Requirement::HostProvided
        );
    }

    #[test]
    fn empty_manifest_denies_every_capability_import() {
        let manifest = Manifest::parse("").unwrap();
        let report = check(
            [
                "wasi:io/streams@0.2.6",
                "wasi:filesystem/types@0.2.6",
                "wasi:sockets/tcp@0.2.6",
                "wasi:clocks/monotonic-clock@0.2.6",
                "wasi:random/random@0.2.6",
                "wasi:cli/environment@0.2.6",
            ]
            .map(callable),
            &manifest.permissions,
            &provided(&[]),
        );

        assert!(!report.is_satisfied());
        assert_eq!(report.denied().count(), 5, "{}", report.describe());
        // The ambient one is still allowed.
        assert!(report.decisions[0].granted);
    }

    #[test]
    fn grants_admit_exactly_what_they_name() {
        let manifest = Manifest::parse(
            r#"
            [permissions]
            fs.read = ["/w"]
            clocks  = "monotonic"
            random  = true
            "#,
        )
        .unwrap();
        let report = check(
            [
                "wasi:filesystem/types@0.2.6",
                "wasi:clocks/monotonic-clock@0.2.6",
                "wasi:random/random@0.2.6",
                "wasi:clocks/wall-clock@0.2.6",
                "wasi:sockets/tcp@0.2.6",
            ]
            .map(callable),
            &manifest.permissions,
            &provided(&[]),
        );

        let denied: Vec<&str> = report.denied().map(|d| d.import.as_str()).collect();
        assert_eq!(
            denied,
            ["wasi:clocks/wall-clock@0.2.6", "wasi:sockets/tcp@0.2.6"],
            "{}",
            report.describe()
        );
    }

    #[test]
    fn wall_clock_grant_also_admits_monotonic() {
        let manifest = Manifest::parse("[permissions]\nclocks = \"wall\"\n").unwrap();
        let report = check(
            [
                "wasi:clocks/wall-clock@0.2.6",
                "wasi:clocks/monotonic-clock@0.2.6",
            ]
            .map(callable),
            &manifest.permissions,
            &provided(&[]),
        );
        assert!(report.is_satisfied(), "{}", report.describe());
    }

    #[test]
    fn a_component_with_no_imports_is_satisfied() {
        let manifest = Manifest::parse("").unwrap();
        let report = check([], &manifest.permissions, &provided(&[]));
        assert!(report.is_satisfied());
        assert_eq!(report.denied().count(), 0);
    }

    #[test]
    fn description_names_the_key_an_operator_would_edit() {
        let manifest = Manifest::parse("").unwrap();
        let report = check(
            ["wasi:sockets/tcp@0.2.6"].map(callable),
            &manifest.permissions,
            &provided(&[]),
        );
        let text = report.describe();
        assert!(text.contains("DENY"), "{text}");
        assert!(text.contains("permissions.net"), "{text}");
    }
}
