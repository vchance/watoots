//! The import-intersection check.
//!
//! At load time we enumerate what a component actually imports and intersect it
//! with what the manifest grants. Anything not covered is a *load* error, not a
//! runtime trap: the point of a manifest is that you learn a plugin wants the
//! network when you install it, not when it first tries to use it.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use crate::manifest::{Clocks, Permissions};

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
    /// `wasi:logging` — needs a `logging` level grant.
    Logging,
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
            Self::Logging => "permissions.logging = \"warn\"",
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
        // Outbound HTTP is network access by another name. A JS or Python guest
        // links it in by default, so this is not a hypothetical.
        ("http", _) => Requirement::Network,
        // `wasi:logging@0.1.0-draft` declares exactly one interface, also called
        // `logging`. Matched by name rather than by package so that a second
        // interface appearing in a later revision denies until we have looked
        // at it, instead of riding in on the grant for this one.
        ("logging", "logging") => Requirement::Logging,
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

    /// The plain-language answer to "what can this plugin actually do?".
    ///
    /// `describe` lists imports; this lists *capabilities*, which is the
    /// question an operator is really asking and the one `docs/SPEC.md` set for
    /// v0.2 — "reads files under X, no network, uses monotonic clock". The two
    /// differ in three ways that matter:
    ///
    /// - **It resolves against the manifest**, so a granted filesystem shows
    ///   the directories rather than the word "granted".
    /// - **It reports what was granted and never asked for.** Over-granting is
    ///   invisible in an import list, because the evidence is an import that is
    ///   not there. It is exactly what a reviewer is looking for.
    /// - **It separates "you must implement this" from "denied".** An interface
    ///   the application is expected to serve is not a permission problem, and
    ///   telling an operator to edit their manifest about it wastes their time.
    #[must_use]
    pub fn summarize(&self, permissions: &Permissions) -> String {
        let mut out = String::new();
        out.push_str("capabilities\n");
        for row in self.capabilities(permissions) {
            let _ = writeln!(out, "  {:<12} {:<6} {}", row.name, row.mark, row.detail);
        }

        let application: Vec<&ImportDecision> = self
            .decisions
            .iter()
            .filter(|decision| {
                matches!(
                    decision.requirement,
                    Requirement::HostProvided | Requirement::Unrecognized
                ) && !decision.import.starts_with("wasi:")
            })
            .collect();
        if !application.is_empty() {
            out.push_str("\nyour application must serve\n");
            for decision in application {
                let note = if decision.requirement == Requirement::HostProvided {
                    "  (already provided)"
                } else {
                    ""
                };
                let _ = writeln!(out, "  {}{note}", decision.import);
            }
        }

        let unknown: Vec<&ImportDecision> = self
            .decisions
            .iter()
            .filter(|decision| {
                decision.requirement == Requirement::Unrecognized
                    && decision.import.starts_with("wasi:")
            })
            .collect();
        if !unknown.is_empty() {
            out.push_str("\nWASI interfaces this host does not implement\n");
            for decision in unknown {
                let _ = writeln!(out, "  {}", decision.import);
            }
        }

        let total = self.decisions.len();
        let denied = self.decisions.iter().filter(|d| !d.granted).count();
        let free = self
            .decisions
            .iter()
            .filter(|d| matches!(d.requirement, Requirement::Ambient | Requirement::TypesOnly))
            .count();
        let _ = write!(
            out,
            "\n{total} import(s): {free} need no grant, {denied} not granted"
        );
        out.push('\n');

        // Worth its own line rather than a column somebody has to scan for: a
        // manifest can satisfy every import and still hand out four
        // capabilities nothing asked for, and "every import is granted" reads
        // like an all-clear.
        let unused = self
            .capabilities(permissions)
            .into_iter()
            .filter(|row| row.granted && !row.requested)
            .count();
        if unused > 0 {
            let _ = writeln!(
                out,
                "{unused} capability(ies) granted but never imported; the manifest can be tightened"
            );
        }
        out
    }

    /// One row per capability, in a fixed order so the shape does not move
    /// between components. A capability nobody asked for still gets a row:
    /// "granted, never used" is the finding.
    #[must_use]
    pub fn capabilities(&self, permissions: &Permissions) -> Vec<CapabilityRow> {
        let wants = |kinds: &[Requirement]| {
            self.decisions
                .iter()
                .any(|decision| kinds.contains(&decision.requirement))
        };

        let clock_detail = match permissions.clocks {
            Clocks::None => String::from("no clock granted"),
            Clocks::Monotonic => String::from("monotonic only - durations, not dates"),
            Clocks::Wall => String::from("monotonic and wall clock"),
        };
        let fs_detail = if permissions.fs.is_empty() {
            String::from("no filesystem granted")
        } else {
            let mut detail = String::new();
            if !permissions.fs.read.is_empty() {
                let _ = write!(detail, "reads {}", permissions.fs.read.join(", "));
            }
            if !permissions.fs.write.is_empty() {
                if !detail.is_empty() {
                    detail.push_str("; ");
                }
                let _ = write!(detail, "writes {}", permissions.fs.write.join(", "));
            }
            detail
        };
        let net_detail = match &permissions.net {
            None => String::from("no sockets, no HTTP"),
            Some(hosts) if hosts.is_empty() => {
                String::from("interfaces linked, every connection refused")
            }
            Some(hosts) => format!("allowlist: {}", hosts.join(", ")),
        };
        let env_detail = match &permissions.env {
            None => String::from("cannot read the environment"),
            Some(vars) if vars.is_empty() => String::from("may read an empty environment"),
            Some(vars) => format!("{} variable(s)", vars.len()),
        };

        vec![
            CapabilityRow::new(
                "filesystem",
                wants(&[Requirement::Filesystem]),
                !permissions.fs.is_empty(),
                fs_detail,
            ),
            CapabilityRow::new(
                "network",
                wants(&[Requirement::Network]),
                permissions.net.is_some(),
                net_detail,
            ),
            CapabilityRow::new(
                "clock",
                wants(&[Requirement::MonotonicClock, Requirement::WallClock]),
                permissions.clocks != Clocks::None,
                clock_detail,
            ),
            CapabilityRow::new(
                "environment",
                wants(&[Requirement::Environment]),
                permissions.env.is_some(),
                env_detail,
            ),
            CapabilityRow::new(
                "random",
                wants(&[Requirement::Random]),
                permissions.random,
                String::from(if permissions.random {
                    "a seeded generator"
                } else {
                    "no random granted"
                }),
            ),
            CapabilityRow::new(
                "logging",
                wants(&[Requirement::Logging]),
                permissions.logging.is_some(),
                match permissions.logging {
                    Some(level) => format!("{} and above", level.as_wit_name()),
                    None => String::from("no logging granted"),
                },
            ),
        ]
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

/// One capability, as `watoots inspect` renders it.
///
/// Four states rather than two, because "asked for and denied" and "granted
/// and never asked for" are different findings with different fixes.
#[derive(Debug, Clone)]
pub struct CapabilityRow {
    /// What the capability is called in the manifest.
    pub name: &'static str,
    /// Whether any import needs it.
    pub requested: bool,
    /// Whether the manifest grants it.
    pub granted: bool,
    /// `ok`, `DENY`, `UNUSED`, or `-`.
    pub mark: &'static str,
    /// What the grant means, in words.
    pub detail: String,
}

impl CapabilityRow {
    fn new(name: &'static str, requested: bool, granted: bool, detail: String) -> Self {
        let (mark, detail) = match (requested, granted) {
            (true, true) => ("ok", detail),
            (true, false) => ("DENY", format!("wanted; {detail}")),
            // The interesting one. An import list cannot show this, because the
            // evidence is an import that is absent.
            (false, true) => ("UNUSED", format!("granted but never imported; {detail}")),
            (false, false) => ("-", String::from("not requested, not granted")),
        };
        Self {
            name,
            requested,
            granted,
            mark,
            detail,
        }
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
        // Granted by the key being present at all. An empty list means the
        // guest may link the socket interfaces and reach nothing through them,
        // which is what a scripting-language runtime needs.
        Requirement::Network => permissions.net.is_some(),
        Requirement::MonotonicClock => permissions.clocks.allows_monotonic(),
        Requirement::WallClock => permissions.clocks.allows_wall(),
        Requirement::Random => permissions.random,
        // `env = {}` is a real grant meaning "an empty environment": the guest
        // may read it and will find nothing. Absent means it may not read at
        // all, which is why this is an Option and not an emptiness check.
        Requirement::Environment => permissions.env.is_some(),
        // Granted by naming a level at all. Which levels actually reach the
        // sink is a filter applied per message; this is only the question of
        // whether the interface links.
        Requirement::Logging => permissions.logging.is_some(),
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

    /// The four capability states, on one report.
    #[test]
    fn capabilities_separate_denied_from_never_asked_for() {
        let manifest: Manifest = toml::from_str(
            r#"
            [permissions]
            clocks = "monotonic"
            random = true
            "#,
        )
        .unwrap();
        let permissions = &manifest.permissions;

        // Wants a clock (granted) and the environment (not granted). Never
        // mentions the filesystem; is handed random it never asked for.
        let imports = [
            callable("wasi:clocks/monotonic-clock@0.2.9"),
            callable("wasi:cli/environment@0.2.9"),
        ];
        let report = check(imports, permissions, &provided(&[]));
        let rows = report.capabilities(permissions);
        let row = |name: &str| {
            rows.iter()
                .find(|row| row.name == name)
                .unwrap_or_else(|| panic!("no {name} row"))
        };

        assert_eq!(row("clock").mark, "ok");
        assert_eq!(row("environment").mark, "DENY");
        assert_eq!(row("filesystem").mark, "-");
        // The one an import list cannot express: the evidence is an absence.
        assert_eq!(row("random").mark, "UNUSED");
    }

    /// A granted filesystem names the directories, which is the whole point of
    /// summarising against the manifest rather than listing imports.
    #[test]
    fn a_granted_filesystem_names_the_directories() {
        let manifest: Manifest = toml::from_str(
            r#"
            [permissions]
            fs.read  = ["/srv/docs"]
            fs.write = ["/tmp/cache"]
            "#,
        )
        .unwrap();
        let imports = [callable("wasi:filesystem/types@0.2.9")];
        let report = check(imports, &manifest.permissions, &provided(&[]));
        let summary = report.summarize(&manifest.permissions);
        assert!(summary.contains("reads /srv/docs"), "{summary}");
        assert!(summary.contains("writes /tmp/cache"), "{summary}");
    }

    /// An application interface is not a permission problem, and must not be
    /// reported as one.
    #[test]
    fn an_application_interface_is_not_reported_as_a_denial() {
        let manifest = Manifest::default();
        let imports = [callable("watoots:example/log@0.1.0")];
        let report = check(imports, &manifest.permissions, &provided(&[]));
        let summary = report.summarize(&manifest.permissions);
        assert!(summary.contains("your application must serve"), "{summary}");
        assert!(summary.contains("watoots:example/log@0.1.0"), "{summary}");
        // No capability row should claim the operator can grant it.
        for row in report.capabilities(&manifest.permissions) {
            assert_ne!(row.mark, "DENY", "{} should not be a denial", row.name);
        }
    }

    /// Every import satisfied is not the same as a tight manifest.
    #[test]
    fn over_granting_is_called_out_even_when_nothing_is_denied() {
        let manifest: Manifest = toml::from_str(
            r#"
            [permissions]
            random = true
            net    = []
            "#,
        )
        .unwrap();
        let imports = [callable("wasi:io/streams@0.2.9")];
        let report = check(imports, &manifest.permissions, &provided(&[]));
        assert!(report.is_satisfied());
        let summary = report.summarize(&manifest.permissions);
        assert!(
            summary.contains("2 capability(ies) granted but never imported"),
            "{summary}"
        );
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
            ("wasi:logging/logging@0.1.0-draft", Requirement::Logging),
        ];
        for (import, expected) in cases {
            assert_eq!(classify(callable(import), &none), expected, "{import}");
        }
    }

    #[test]
    fn http_counts_as_network() {
        assert_eq!(
            classify(
                callable("wasi:http/outgoing-handler@0.2.10"),
                &provided(&[])
            ),
            Requirement::Network
        );
    }

    #[test]
    fn a_prerelease_version_suffix_still_parses_and_matches_unversioned() {
        // `wasi:logging` is Phase 1 and ships as `0.1.0-draft`. The hyphen has
        // to survive parsing, or the grant is matched against a name nobody
        // ever writes.
        let iface = InterfaceRef::parse("wasi:logging/logging@0.1.0-draft").unwrap();
        assert_eq!(iface.version.as_deref(), Some("0.1.0-draft"));
        assert_eq!(iface.unversioned(), "wasi:logging/logging");
    }

    #[test]
    fn logging_is_granted_by_naming_a_level_and_denied_by_saying_nothing() {
        let denied = Manifest::parse("").unwrap();
        let report = check(
            ["wasi:logging/logging@0.1.0-draft"].map(callable),
            &denied.permissions,
            &provided(&[]),
        );
        assert!(!report.is_satisfied(), "{}", report.describe());
        assert!(
            report.describe().contains("permissions.logging"),
            "{}",
            report.describe()
        );

        let granted = Manifest::parse("[permissions]\nlogging = \"critical\"\n").unwrap();
        let report = check(
            ["wasi:logging/logging@0.1.0-draft"].map(callable),
            &granted.permissions,
            &provided(&[]),
        );
        // Even the strictest ceiling grants the interface: what it filters is
        // which messages reach the sink, not whether the component links.
        assert!(report.is_satisfied(), "{}", report.describe());
    }

    #[test]
    fn a_second_interface_in_the_logging_package_is_not_covered_by_the_grant() {
        let granted = Manifest::parse("[permissions]\nlogging = \"trace\"\n").unwrap();
        assert_eq!(
            classify(callable("wasi:logging/handler@0.2.0"), &provided(&[])),
            Requirement::Unrecognized
        );
        let report = check(
            ["wasi:logging/handler@0.2.0"].map(callable),
            &granted.permissions,
            &provided(&[]),
        );
        assert!(!report.is_satisfied(), "{}", report.describe());
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
