//! The `watoots` command line tool.
//!
//! Inspect what a plugin would be granted, run one, record a session, and
//! replay a recording in CI. One module per family of subcommands:
//!
//! - [`inspect`]: `inspect`, `diff`, `wit semver-check` -- questions asked of a
//!   component without running it.
//! - [`run`]: `run`, `profile`, `reload` -- calls into a plugin.
//! - [`record`]: `record`, `replay`, `trace fmt` -- the trace file.
//! - [`fuzz`]: `fuzz` -- the campaign form of the property tests.

mod fuzz;
mod inspect;
mod record;
mod run;

use std::fmt;
use std::path::Path;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use watoots::Manifest;

#[derive(Parser)]
#[command(name = "watoots", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Explain a component's imports as a permission grant list.
    Inspect(inspect::InspectArgs),
    /// Call an exported function.
    Run(run::RunArgs),
    /// Call an exported function and report where its time went.
    Profile(run::ProfileArgs),
    /// Call an export, swap the component for a new build, and call it again.
    Reload(run::ReloadArgs),
    /// Call an exported function and write a trace of every crossing.
    Record(record::RecordArgs),
    /// Re-run a recorded session against a component, with no application.
    Replay(record::ReplayArgs),
    /// Call a plugin with generated arguments and check it records and replays.
    Fuzz(fuzz::FuzzArgs),
    /// Compare two builds of a plugin: what changed, and would it still load.
    Diff(inspect::DiffArgs),
    /// Convert a trace between the text and binary encodings.
    #[command(subcommand)]
    Trace(record::TraceCommand),
    /// Compare two WIT packages for compatibility.
    #[command(subcommand)]
    Wit(inspect::WitCommand),
}

fn main() -> ExitCode {
    match dispatch(Cli::parse()) {
        Ok(code) => code,
        Err(failure) => {
            eprintln!("watoots: {failure}");
            ExitCode::FAILURE
        }
    }
}

fn dispatch(cli: Cli) -> Outcome {
    match cli.command {
        Command::Inspect(args) => inspect::inspect(&args),
        Command::Run(args) => run::run(&args),
        Command::Profile(args) => run::profile(&args),
        Command::Reload(args) => run::reload(&args),
        Command::Record(args) => record::record(&args),
        Command::Replay(args) => record::replay(&args),
        Command::Fuzz(args) => fuzz::fuzz(&args),
        Command::Diff(args) => inspect::diff(&args),
        Command::Wit(inspect::WitCommand::SemverCheck(args)) => inspect::semver_check(&args),
        Command::Trace(command) => record::trace(command),
    }
}

// ---------------------------------------------------------------------------
// What every subcommand shares
// ---------------------------------------------------------------------------

/// Why a subcommand could not finish: one line for stderr.
///
/// Every library error this tool meets collapses to its message, so `?` works
/// on a `watoots::Result`, a `watoots_trace::Result` and a `String` alike and
/// `main` prints whichever arrived. A subcommand that *ran* and found something
/// -- a divergence, a denial -- is not a failure in this sense; it returns a
/// non-zero `ExitCode` after saying so on stdout.
pub(crate) struct Failure(String);

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<watoots::Error> for Failure {
    fn from(err: watoots::Error) -> Self {
        Self(err.message().to_string())
    }
}

impl From<watoots_trace::Error> for Failure {
    fn from(err: watoots_trace::Error) -> Self {
        Self(err.message().to_string())
    }
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self(message)
    }
}

impl From<&str> for Failure {
    fn from(message: &str) -> Self {
        Self(message.to_string())
    }
}

/// What a subcommand returns: a failure, or the exit code of a command that
/// ran to the end.
pub(crate) type Outcome = Result<ExitCode, Failure>;

/// Success when `ok`, failure otherwise -- for the subcommands whose answer is
/// a verdict rather than an error.
pub(crate) fn verdict(ok: bool) -> Outcome {
    Ok(if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

pub(crate) fn read(path: &Path) -> Result<Vec<u8>, Failure> {
    std::fs::read(path).map_err(|err| format!("cannot read {}: {err}", path.display()).into())
}

pub(crate) fn read_text(path: &Path) -> Result<String, Failure> {
    std::fs::read_to_string(path)
        .map_err(|err| format!("cannot read {}: {err}", path.display()).into())
}

pub(crate) fn write(path: &Path, bytes: impl AsRef<[u8]>) -> Result<(), Failure> {
    std::fs::write(path, bytes)
        .map_err(|err| format!("cannot write {}: {err}", path.display()).into())
}

/// The manifest to run under, and its text for whoever needs to carry it (a
/// trace header, a fuzz session). Read once, so the two cannot disagree. No
/// path means the default manifest, which denies everything.
pub(crate) fn read_manifest(path: Option<&Path>) -> Result<(Manifest, String), Failure> {
    match path {
        None => Ok((Manifest::default(), String::new())),
        Some(path) => {
            let text = read_text(path)?;
            let manifest = Manifest::parse(&text)
                .map_err(|err| format!("{}: {}", path.display(), err.message()))?;
            Ok((manifest, text))
        }
    }
}

/// The name a component is known by: its file stem, as `Host::load` names it.
pub(crate) fn plugin_name(component: &Path) -> String {
    component.file_stem().map_or_else(
        || "plugin".to_string(),
        |stem| stem.to_string_lossy().into(),
    )
}
