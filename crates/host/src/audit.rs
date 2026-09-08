//! The observation seam a security reviewer reads.
//!
//! [`TraceHook`](crate::TraceHook) answers *what happened*: every crossing, as
//! typed values, for record and replay. This answers a different question —
//! *what was this plugin permitted to do, and what was it refused* — and it is a
//! sibling of that hook rather than more variants on it. The two differ in every
//! dimension that matters: the audience, the volume, how long the answer is kept
//! and, decisively, what the events carry.
//!
//! **An audit event carries names and verdicts, never argument values.** A trace
//! holds every value that crossed the boundary, which is the plugin's data and
//! often the user's; `.github/ISSUE_TEMPLATE` warns a reporter to read one
//! before pasting it. An audit trail has to be the thing you *can* keep, so a
//! suppressed log line here says that a line was suppressed and at what level,
//! and does not carry the line. See `docs/adr/0011-audit-trail.md`.
//!
//! Every event originates on the host side of the boundary, at the point the
//! decision is made. A guest can cause an event and can neither suppress nor
//! forge one — the same property [`PluginStats`](crate::PluginStats) has, for
//! the same reason.
//!
//! Nothing is installed by default. An embedding application that registers no
//! hook gets no audit trail; `docs/SECURITY.md` says so out loud, because a
//! reader who assumes otherwise assumes it in the direction that hurts.

use std::fmt;

use crate::imports::{ImportDecision, Requirement};
use crate::manifest::LogLevel;

/// How one import resolved at load.
///
/// Four states rather than two, because "the manifest granted it", "the
/// application serves it itself" and "there was nothing there to grant" are
/// different answers that all read as *allowed* in a yes/no column — and only
/// one of them is a capability the manifest handed out.
///
/// Deliberately *not* `#[non_exhaustive]`, for the reason given on
/// [`AuditEvent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// The manifest covers it. Includes the WASI plumbing every component pulls
    /// in, which needs no grant.
    Granted,
    /// The manifest does not cover it, so the load is refused.
    Denied,
    /// The embedding application declared it serves this interface, so the
    /// manifest was never consulted about it.
    HostProvided,
    /// An interface imported only for its type definitions. Nothing there is
    /// callable, so nothing there is a capability.
    TypesOnly,
}

impl Verdict {
    /// Read the verdict off a decision the grant check already made.
    ///
    /// This reports what [`check`](crate::imports::check) decided; it does not
    /// decide anything itself. Adding an arm here must never change who gets
    /// loaded.
    #[must_use]
    pub fn of(decision: &ImportDecision) -> Self {
        match decision.requirement {
            Requirement::HostProvided => Self::HostProvided,
            Requirement::TypesOnly => Self::TypesOnly,
            _ if decision.granted => Self::Granted,
            _ => Self::Denied,
        }
    }

    /// Stable spelling, as the rendered line and the C surface use it.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Granted => "granted",
            Self::Denied => "denied",
            Self::HostProvided => "host-provided",
            Self::TypesOnly => "types-only",
        }
    }
}

/// A ceiling from the manifest's `[limits]` table.
///
/// Which limit was spent, never what was being carried when it went: the number
/// of bytes a plugin tried to log is a fact about the plugin, the bytes
/// themselves are its data.
///
/// Deliberately *not* `#[non_exhaustive]`, for the reason given on
/// [`AuditEvent`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ceiling {
    /// `limits.fuel` — the guest ran out of instructions.
    Fuel,
    /// `limits.timeout` — the per-call epoch deadline fired.
    Timeout,
    /// `limits.memory` — a linear-memory growth was refused.
    ///
    /// The only one of these a plugin never sees as an error: a refused growth
    /// makes `memory.grow` return `-1`, so without this event the fact that a
    /// plugin hit its memory ceiling reaches nobody at all.
    Memory,
    /// `limits.transfer` — one crossing tried to hand the host more than it may
    /// allocate on the guest's behalf.
    Transfer,
    /// `limits.log_bytes` — the call's log volume budget.
    LogBytes,
    /// `limits.log_messages` — the call's log message budget.
    LogMessages,
}

impl Ceiling {
    /// The manifest key an operator would edit, e.g. `"limits.fuel"`.
    #[must_use]
    pub fn manifest_key(self) -> &'static str {
        match self {
            Self::Fuel => "limits.fuel",
            Self::Timeout => "limits.timeout",
            Self::Memory => "limits.memory",
            Self::Transfer => "limits.transfer",
            Self::LogBytes => "limits.log_bytes",
            Self::LogMessages => "limits.log_messages",
        }
    }
}

/// One authorisation decision, as it was made.
///
/// Authorisation decisions and only those. Not crossings: a caller who wants
/// those wants [`TraceHook`](crate::TraceHook), and pointing them at it is a
/// better answer than making this the same thing twice.
///
/// Deliberately *not* `#[non_exhaustive]`, for the same reason as
/// [`TraceEvent`](crate::TraceEvent). Marking it so would let us add a decision
/// later without a major version bump, at the cost of forcing every implementor
/// to write a `_ => {}` arm — and for an audit trail, that arm means silently
/// dropping the decision nobody wrote down. A compile error is the better
/// failure: a new capability that does not emit here is a hole, and adding a
/// variant should make every auditor say so.
#[derive(Debug)]
pub enum AuditEvent<'a> {
    /// A plugin was compiled, granted, and instantiated.
    Loaded {
        /// The name it was loaded under.
        plugin: &'a str,
        /// SHA-256 of the component bytes, lowercase hex. *Which* bytes is the
        /// first question after an incident, and the only one the plugin's name
        /// cannot answer.
        sha256: &'a str,
        /// How many imports the component declares.
        imports: usize,
    },
    /// A plugin ran without its publisher being checked.
    ///
    /// Emitted on every load and reload under a manifest with no trusted keys,
    /// which is a decision in exactly ADR-0011's sense: the host decided not to
    /// ask who wrote this code. It carries no argument values, so it is safe to
    /// keep for as long as an incident takes.
    ///
    /// Loud on purpose. The permission model can say what a plugin may do and
    /// cannot say that the plugin is the one you think it is — swap the file on
    /// disk and the replacement inherits every grant the manifest gave. This
    /// event is the only record that nothing stood between those two cases.
    LoadedUnverified {
        /// The name it was loaded under.
        plugin: &'a str,
        /// SHA-256 of the bytes that ran, lowercase hex. With no signature to
        /// vouch for them this is the only identity they have.
        sha256: &'a str,
    },
    /// A load was refused, and the plugin never ran.
    LoadRefused {
        /// The name it would have been loaded under.
        plugin: &'a str,
        /// SHA-256 of the bytes that were refused.
        sha256: &'a str,
        /// The import that decided it, when one did. `None` for a refusal that
        /// is not about permissions — bytes that are not a component, a
        /// manifest that would not expand, an instantiation that failed.
        import: Option<&'a str>,
        /// What that import needed.
        requirement: Option<Requirement>,
        /// The failure, in words. May be several lines; the rendering in
        /// [`Display`](fmt::Display) keeps only the first.
        reason: &'a str,
    },
    /// One import's verdict, as the grant check resolved it.
    ///
    /// Emitted for every import on every load *and* every reload, before the
    /// load's own verdict, so a refusal arrives already explained.
    ImportDecided {
        /// The plugin the check was run for.
        plugin: &'a str,
        /// The import name exactly as the component declares it, version
        /// included.
        import: &'a str,
        /// What it needed.
        requirement: Requirement,
        /// How it resolved.
        verdict: Verdict,
    },
    /// A plugin's code was replaced.
    ///
    /// Separate from [`Loaded`](AuditEvent::Loaded) because a reload is a
    /// separate decision: new bytes re-run the whole grant check, and must not
    /// be able to acquire a capability by arriving as an update.
    Reloaded {
        /// The plugin, which keeps the name it was loaded under.
        plugin: &'a str,
        /// SHA-256 of the bytes now running.
        sha256: &'a str,
        /// Reloads this plugin has survived, this one included.
        reloads: u64,
    },
    /// A reload was refused, and the plugin is still running its old code.
    ReloadRefused {
        /// The plugin that was not replaced.
        plugin: &'a str,
        /// SHA-256 of the bytes that were refused.
        sha256: &'a str,
        /// The import that decided it, when one did.
        import: Option<&'a str>,
        /// What that import needed.
        requirement: Option<Requirement>,
        /// The failure, in words. First line only in the rendering.
        reason: &'a str,
    },
    /// A `wasi:logging` line passed the manifest's level ceiling.
    ///
    /// That it reached a sink is a separate question: a grant with no sink is
    /// legal and discards.
    LogAdmitted {
        /// The plugin that logged.
        plugin: &'a str,
        /// The level it logged at.
        level: LogLevel,
    },
    /// A `wasi:logging` line was dropped by the manifest's level ceiling.
    ///
    /// The event this hook exists for. "The plugin said nothing" and "the
    /// plugin was not allowed to say it" have been indistinguishable, and they
    /// mean opposite things.
    ///
    /// **It does not carry the line.** That is the constraint the whole hook is
    /// built around; the message body belongs to a trace, which is a thing you
    /// read before you paste it.
    LogSuppressed {
        /// The plugin that was not heard.
        plugin: &'a str,
        /// The level it logged at, or `None` when it named a `level` case this
        /// build does not define — which is dropped rather than delivered, so
        /// that a manifest's ceiling holds across a revision of a Phase 1
        /// proposal that adds a case.
        level: Option<LogLevel>,
        /// The manifest's ceiling, which is what dropped it.
        ceiling: LogLevel,
    },
    /// A ceiling was spent, and the call it happened in was stopped.
    CeilingSpent {
        /// The plugin that hit it.
        plugin: &'a str,
        /// Which one.
        limit: Ceiling,
    },
}

impl AuditEvent<'_> {
    /// The plugin this event is about.
    #[must_use]
    pub fn plugin(&self) -> &str {
        match self {
            Self::Loaded { plugin, .. }
            | Self::LoadedUnverified { plugin, .. }
            | Self::LoadRefused { plugin, .. }
            | Self::ImportDecided { plugin, .. }
            | Self::Reloaded { plugin, .. }
            | Self::ReloadRefused { plugin, .. }
            | Self::LogAdmitted { plugin, .. }
            | Self::LogSuppressed { plugin, .. }
            | Self::CeilingSpent { plugin, .. } => plugin,
        }
    }

    /// The stable spelling of this decision, e.g. `"log-suppressed"`.
    ///
    /// Leads the rendered line and names the case on the C surface, so it is
    /// part of the API rather than a debugging convenience.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Loaded { .. } => "loaded",
            Self::LoadedUnverified { .. } => "loaded-unverified",
            Self::LoadRefused { .. } => "load-refused",
            Self::ImportDecided { .. } => "import",
            Self::Reloaded { .. } => "reloaded",
            Self::ReloadRefused { .. } => "reload-refused",
            Self::LogAdmitted { .. } => "log-admitted",
            Self::LogSuppressed { .. } => "log-suppressed",
            Self::CeilingSpent { .. } => "ceiling-spent",
        }
    }
}

/// One line, `key=value` after the decision's name.
///
/// A rendering rather than a format: it is what the C surface hands over
/// alongside the structured fields, and what the CLI writes to stderr. It stays
/// on one line even when a `reason` does not, because an audit line with a guest
/// backtrace folded into it is not one a reader can scan.
impl fmt::Display for AuditEvent<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Only ever the first line: `reason` is a host error message, and a
        // wasmtime one carries a backtrace under its root cause.
        fn head(reason: &str) -> &str {
            reason.lines().next().unwrap_or("")
        }

        write!(f, "{} plugin={}", self.name(), self.plugin())?;
        match self {
            Self::Loaded {
                sha256, imports, ..
            } => write!(f, " sha256={sha256} imports={imports}"),
            // The risk is spelled out rather than left to the reader. This line
            // is the one a security reviewer is most likely to be scanning for,
            // and "loaded-unverified" alone does not say why it matters.
            Self::LoadedUnverified { sha256, .. } => write!(
                f,
                " sha256={sha256} risk=publisher-unchecked \
                 (no signature.keys in the manifest, so substituted bytes would \
                 load with every grant this policy gives)"
            ),
            Self::LoadRefused {
                sha256,
                import,
                requirement,
                reason,
                ..
            }
            | Self::ReloadRefused {
                sha256,
                import,
                requirement,
                reason,
                ..
            } => {
                write!(f, " sha256={sha256}")?;
                if let Some(import) = import {
                    write!(f, " import={import}")?;
                }
                if let Some(requirement) = requirement {
                    write!(f, " grant={}", requirement.grant_key())?;
                }
                write!(f, " reason={}", head(reason))
            }
            Self::ImportDecided {
                import,
                requirement,
                verdict,
                ..
            } => write!(
                f,
                " import={import} verdict={} requirement={}",
                verdict.name(),
                requirement.name()
            ),
            Self::Reloaded {
                sha256, reloads, ..
            } => write!(f, " sha256={sha256} reloads={reloads}"),
            Self::LogAdmitted { level, .. } => write!(f, " level={}", level.as_wit_name()),
            Self::LogSuppressed { level, ceiling, .. } => write!(
                f,
                " level={} ceiling={}",
                level.map_or("<unknown>", LogLevel::as_wit_name),
                ceiling.as_wit_name()
            ),
            Self::CeilingSpent { limit, .. } => write!(f, " limit={}", limit.manifest_key()),
        }
    }
}

/// Observes every authorisation decision the host makes.
///
/// A hook must not panic and should be cheap: it runs inline at the decision,
/// and may be entered from whatever thread made the call — a `wasi:logging`
/// verdict is reached inside the guest's own call.
///
/// Installed with [`HostBuilder::audit_hook`](crate::HostBuilder::audit_hook).
/// There is no default: a library that writes to stderr uninvited is badly
/// behaved, and the application already has a logging system that is better than
/// one we would ship.
pub trait AuditHook: Send + Sync {
    /// Called once per decision, at the point it is made.
    fn on_event(&self, event: &AuditEvent<'_>);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::imports::Requirement;

    fn decision(import: &str, requirement: Requirement, granted: bool) -> ImportDecision {
        ImportDecision {
            import: import.to_string(),
            requirement,
            granted,
        }
    }

    #[test]
    fn a_verdict_reads_the_decision_rather_than_re_deciding_it() {
        let cases = [
            (Requirement::Ambient, true, Verdict::Granted),
            (Requirement::Filesystem, true, Verdict::Granted),
            (Requirement::Filesystem, false, Verdict::Denied),
            (Requirement::Unrecognized, false, Verdict::Denied),
            // Neither of these is a grant the manifest handed out, and both
            // read as "allowed" in a yes/no column.
            (Requirement::HostProvided, true, Verdict::HostProvided),
            (Requirement::TypesOnly, true, Verdict::TypesOnly),
        ];
        for (requirement, granted, expected) in cases {
            assert_eq!(
                Verdict::of(&decision("x:y/z", requirement, granted)),
                expected,
                "{requirement:?} granted={granted}"
            );
        }
    }

    #[test]
    fn a_rendered_line_stays_one_line_however_many_the_reason_has() {
        let event = AuditEvent::LoadRefused {
            plugin: "lint",
            sha256: "abc",
            import: Some("wasi:sockets/tcp@0.2.6"),
            requirement: Some(Requirement::Network),
            reason: "1 import(s) not granted\n  DENY wasi:sockets/tcp\n  more\n",
        };
        let line = event.to_string();
        assert!(!line.contains('\n'), "{line}");
        assert!(line.contains("import=wasi:sockets/tcp@0.2.6"), "{line}");
        assert!(line.contains("grant=permissions.net"), "{line}");
    }

    #[test]
    fn a_suppressed_line_names_the_level_and_never_the_line() {
        let event = AuditEvent::LogSuppressed {
            plugin: "lint",
            level: Some(LogLevel::Trace),
            ceiling: LogLevel::Warn,
        };
        assert_eq!(
            event.to_string(),
            "log-suppressed plugin=lint level=trace ceiling=warn"
        );

        // A case this build does not know is dropped, and says so rather than
        // guessing a level it might have been.
        let unknown = AuditEvent::LogSuppressed {
            plugin: "lint",
            level: None,
            ceiling: LogLevel::Warn,
        };
        assert!(unknown.to_string().contains("level=<unknown>"));
    }

    #[test]
    fn every_ceiling_names_a_manifest_key() {
        for limit in [
            Ceiling::Fuel,
            Ceiling::Timeout,
            Ceiling::Memory,
            Ceiling::Transfer,
            Ceiling::LogBytes,
            Ceiling::LogMessages,
        ] {
            assert!(
                limit.manifest_key().starts_with("limits."),
                "{limit:?} does not name a key an operator could edit"
            );
        }
    }
}
