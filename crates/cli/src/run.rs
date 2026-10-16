//! Calling into a plugin: `run`, `profile` and `reload`, and the host they
//! share.

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use clap::Args;
use watoots::{
    AuditEvent, AuditHook, FunctionKind, FunctionProfile, Host, HostBuilder, Manifest, Plugin,
    PluginProfile, PluginStats, Profiling, TraceHook,
};

use crate::{Failure, Outcome, read_manifest};

/// What every subcommand that calls a plugin shares.
#[derive(Args, Clone)]
pub(crate) struct Invocation {
    /// The component to load.
    pub(crate) component: std::path::PathBuf,
    /// Manifest granting what the component imports.
    #[arg(short, long)]
    pub(crate) manifest: Option<std::path::PathBuf>,
    /// Exported function to call.
    #[arg(short, long)]
    call: String,
    /// Write every authorisation decision to stderr: each import's verdict, the
    /// load, log lines the manifest's level ceiling dropped, and any `[limits]`
    /// ceiling spent.
    ///
    /// stderr rather than stdout, because stdout carries the call's return
    /// value and `watoots run ... > out` has to keep working.
    #[arg(long)]
    audit: bool,
    /// Arguments, as WAVE text: `'"notes.md"'`, `42`, `{line: 1}`.
    #[arg(trailing_var_arg = true)]
    args: Vec<String>,
    /// What a host function should answer, as
    /// `<interface>#<function>=<wave>`. Repeat for each one. A function the
    /// plugin calls but that is not listed here answers with nothing, which is
    /// correct for a void function and an error for any other.
    #[arg(long = "answer", value_name = "IFACE#FUNC=WAVE")]
    answers: Vec<String>,
}

#[derive(Args)]
pub(crate) struct RunArgs {
    #[command(flatten)]
    invocation: Invocation,
}

/// `watoots profile`: the three buckets, and optionally the guest's own stacks.
///
/// Deliberately not a flag on `run`. Profiling changes timing, so a profiled
/// run is not the run `run` reproduces -- and it cannot be combined with
/// `record` at all, which a flag would invite. See ADR-0009.
#[derive(Args)]
pub(crate) struct ProfileArgs {
    #[command(flatten)]
    invocation: Invocation,
    /// Repeat the call this many times before reporting. One call is usually
    /// all boundary crossing and no guest, so a hot loop is what makes the
    /// split legible.
    #[arg(long, default_value_t = 1, value_name = "N")]
    repeat: u32,
    /// Also write a Firefox Profiler JSON of sampled guest stacks. Load it at
    /// <https://profiler.firefox.com/>.
    #[arg(long, value_name = "FILE")]
    firefox: Option<std::path::PathBuf>,
    /// How often to sample guest stacks, in milliseconds. Rounded up to the
    /// 1ms epoch tick, and clamped by `limits.timeout`, which always wins.
    #[arg(long, default_value_t = 1, value_name = "MS", requires = "firefox")]
    sample_interval_ms: u64,
}

/// `watoots reload`: does my new build drop in?
///
/// `watoots inspect new.wasm -m policy.toml` already answers the security half
/// -- whether the replacement asks for anything the manifest does not grant --
/// and answers it without running anything. This answers the rest of the
/// question, which nothing else can: that the replacement instantiates under
/// that manifest, that its `save-state`/`restore-state` line up with the
/// outgoing build's, and that the state actually arrives. The call is made
/// before and after so the difference is visible in the output rather than
/// asserted in prose.
#[derive(Args)]
pub(crate) struct ReloadArgs {
    #[command(flatten)]
    invocation: Invocation,
    /// The replacement component.
    #[arg(long = "to", value_name = "COMPONENT")]
    to: std::path::PathBuf,
}

/// Say plainly, on stderr, that nothing checked who wrote this plugin.
///
/// Not behind `--audit`. The audit trail is a record for later; this is a
/// warning for the person running the command now, and a warning nobody sees
/// by default is the theatre ADR-0011 argues against. stderr rather than
/// stdout, because stdout carries the call's return value.
///
/// It names the risk rather than the setting. "signature verification is off"
/// tells a reader nothing they can act on; "anyone who can replace this file
/// gets everything the policy grants" tells them what it costs.
fn warn_if_unverified(manifest: &Manifest, component: &Path) {
    if manifest.signature.is_required() {
        return;
    }
    // The component is named once, by its file name rather than its full path:
    // repeating a long path three times buries the sentence that matters.
    let name = component.file_name().map_or_else(
        || component.display().to_string(),
        |n| n.to_string_lossy().into(),
    );
    // Only claim the policy opted in to this if it actually did. A manifest
    // built inline states no posture at all, and telling someone their file
    // says something it does not is worse than saying nothing.
    let posture = if manifest.signature.required == Some(false) {
        "The policy says `required = false`, so this is deliberate."
    } else {
        "No policy file stated a signature posture."
    };
    eprintln!(
        "warning: {name} is running unverified -- nothing checked who produced it.\n\
         \x20 The sandbox still holds: the plugin gets only what the policy grants. But\n\
         \x20 anyone able to replace that file gets all of it, and nothing here can tell a\n\
         \x20 replacement from the original.\n\
         \x20 To fix: cosign sign-blob --key cosign.key --output-signature {name}.sig {name}\n\
         \x20 then list the public key under [signature] in the policy.\n\
         \x20 {posture}"
    );
}

/// One `--answer iface#func=wave` flag.
struct Answer {
    interface: String,
    func: String,
    value: Option<String>,
}

fn parse_answers(raw: &[String]) -> Result<Vec<Answer>, Failure> {
    raw.iter()
        .map(|entry| {
            let malformed = || {
                Failure::from(format!(
                    "--answer needs <interface>#<function>=<wave>: {entry:?}"
                ))
            };
            let (target, value) = entry.split_once('=').ok_or_else(malformed)?;
            let (interface, func) = target.split_once('#').ok_or_else(malformed)?;
            Ok(Answer {
                interface: interface.to_string(),
                func: func.to_string(),
                value: (!value.is_empty()).then(|| value.to_string()),
            })
        })
        .collect()
}

/// `--audit`: every authorisation decision, one per line, on stderr.
///
/// The CLI is where the destination is not the application's to choose, so it
/// chooses one -- and it is stderr for the same reason the log sink's is:
/// stdout carries the call's return value, and a `watoots run ... > out` has
/// to keep working.
struct StderrAudit;

impl AuditHook for StderrAudit {
    fn on_event(&self, event: &AuditEvent<'_>) {
        eprintln!("audit: {event}");
    }
}

/// Build a host that serves whatever the caller said to answer with.
///
/// Takes the manifest already read, so a caller that also needs its text (the
/// recorder puts it in the trace header) reads the file exactly once and the
/// two cannot disagree.
pub(crate) fn build_host(
    invocation: &Invocation,
    manifest: Manifest,
    hook: Option<Arc<dyn TraceHook>>,
    profiling: Option<Profiling>,
) -> Result<Host, Failure> {
    // Every subcommand that actually runs a plugin comes through here, so this
    // is the one place the warning has to be for none of them to miss it.
    warn_if_unverified(&manifest, &invocation.component);
    let mut builder: HostBuilder = Host::builder().manifest(manifest);

    // `record` and `profile` are separate subcommands precisely so this cannot
    // be asked for; the host would refuse it anyway.
    builder = match profiling {
        None => builder,
        Some(Profiling {
            sample_interval: None,
        }) => builder.profile(),
        Some(Profiling {
            sample_interval: Some(interval),
        }) => builder.profile_guest_samples(interval),
    };

    if let Some(dir) = invocation.component.parent() {
        builder = builder.var("plugin_dir", dir.display().to_string());
    }

    for answer in parse_answers(&invocation.answers)? {
        let value = answer.value.clone();
        let label = format!("{}#{}", answer.interface, answer.func);
        builder = builder.host_func(&answer.interface, &answer.func, move |call| {
            match (&value, call.result_types().first()) {
                (None, _) => Ok(Vec::new()),
                (Some(text), Some(ty)) => Ok(vec![watoots::from_wave(ty, text)?]),
                (Some(_), None) => Err(watoots::Error::invalid_argument(format!(
                    "--answer gives {label} a value, but it returns nothing"
                ))),
            }
        });
    }

    // A plugin granted `permissions.logging` has to be able to reach somebody,
    // and stderr is the only answer a CLI can give: stdout carries the call's
    // return value, and a `watoots run ... > out` has to keep working.
    builder = builder.log_sink(|record| {
        eprintln!(
            "[{}] {}: {}",
            record.level().as_wit_name(),
            record.context(),
            record.message()
        );
    });

    if let Some(hook) = hook {
        builder = builder.trace_hook(hook);
    }
    if invocation.audit {
        builder = builder.audit_hook(Arc::new(StderrAudit) as Arc<dyn AuditHook>);
    }
    Ok(builder.build()?)
}

/// Build the host from the manifest on disk and load the component from its
/// path: what `run`, `profile` and `reload` do before anything else.
fn load(
    invocation: &Invocation,
    hook: Option<Arc<dyn TraceHook>>,
    profiling: Option<Profiling>,
) -> Result<Plugin, Failure> {
    let (manifest, _) = read_manifest(invocation.manifest.as_deref())?;
    let host = build_host(invocation, manifest, hook, profiling)?;
    Ok(host.load(&invocation.component)?)
}

/// Make the invocation's call and print each returned value on its own line.
///
/// stdout carries nothing else, so `watoots run ... > out` is the value.
pub(crate) fn call_and_print(
    plugin: &mut Plugin,
    invocation: &Invocation,
) -> Result<Vec<String>, Failure> {
    let args: Vec<&str> = invocation.args.iter().map(String::as_str).collect();
    let results = plugin.call_wave(&invocation.call, &args)?;
    for value in &results {
        println!("{value}");
    }
    Ok(results)
}

pub(crate) fn run(args: &RunArgs) -> Outcome {
    let mut plugin = load(&args.invocation, None, None)?;
    call_and_print(&mut plugin, &args.invocation)?;
    Ok(ExitCode::SUCCESS)
}

/// `watoots reload`: call, swap the component, call again.
pub(crate) fn reload(args: &ReloadArgs) -> Outcome {
    let mut plugin = load(&args.invocation, None, None)?;
    call_and_print(&mut plugin, &args.invocation)?;

    // The report goes to stderr, as `run`'s log lines do: stdout carries the
    // call's return values and nothing else, so `watoots reload ... > out`
    // keeps working.
    let report = plugin.reload_from_file(&args.to)?;
    eprintln!(
        "reloaded {} as {} (reload {}); state: {}",
        args.invocation.component.display(),
        args.to.display(),
        report.reloads,
        match (report.state_saved, report.state_restored) {
            (_, true) => "carried",
            (true, false) => "DROPPED — the replacement exports no restore-state",
            (false, false) => "none — neither build exports the hooks",
        }
    );

    call_and_print(&mut plugin, &args.invocation)?;
    Ok(ExitCode::SUCCESS)
}

/// `watoots profile`: call a plugin and say where the time went.
pub(crate) fn profile(args: &ProfileArgs) -> Outcome {
    if args.repeat == 0 {
        return Err("--repeat must be at least 1".into());
    }

    let profiling = Profiling {
        sample_interval: args
            .firefox
            .is_some()
            .then(|| std::time::Duration::from_millis(args.sample_interval_ms.max(1))),
    };
    let mut plugin = load(&args.invocation, None, Some(profiling))?;

    // Every repetition but the last is silent; the last prints its return value
    // to stdout as `run` would, so a profiled call stays a call. The report
    // follows it.
    let call_args: Vec<&str> = args.invocation.args.iter().map(String::as_str).collect();
    for _ in 1..args.repeat {
        plugin.call_wave(&args.invocation.call, &call_args)?;
    }
    call_and_print(&mut plugin, &args.invocation)?;

    let profile = plugin.profile()?;
    print!("{}", render_profile(&profile, &plugin.stats()));

    if let Some(path) = &args.firefox {
        plugin.write_guest_profile(path)?;
        eprintln!(
            "wrote {} — open it at https://profiler.firefox.com/",
            path.display()
        );
    }

    Ok(ExitCode::SUCCESS)
}

// ---------------------------------------------------------------------------
// Rendering the profile
// ---------------------------------------------------------------------------

/// A byte count at a scale a human reads without counting zeroes.
fn bytes(count: u64) -> String {
    const UNITS: [(u64, &str); 3] = [(1 << 30, "GiB"), (1 << 20, "MiB"), (1 << 10, "KiB")];
    for (scale, suffix) in UNITS {
        if count >= scale {
            return format!("{:.1}{suffix}", count as f64 / scale as f64);
        }
    }
    format!("{count}B")
}

/// A duration in nanoseconds, likewise.
fn duration(nanos: u64) -> String {
    match nanos {
        n if n < 1_000 => format!("{n}ns"),
        n if n < 1_000_000 => format!("{:.3}µs", n as f64 / 1_000.0),
        n if n < 1_000_000_000 => format!("{:.3}ms", n as f64 / 1_000_000.0),
        n => format!("{:.3}s", n as f64 / 1_000_000_000.0),
    }
}

fn percent(part: u64, whole: u64) -> String {
    if whole == 0 {
        return "  0.0%".to_string();
    }
    format!("{:5.1}%", (part as f64 / whole as f64) * 100.0)
}

fn render_profile(profile: &PluginProfile, stats: &PluginStats) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();

    let _ = writeln!(out, "\n{} call(s)", profile.calls);
    for (label, nanos) in [
        ("guest", profile.guest_nanos),
        ("host call", profile.host_nanos),
        ("marshalling", profile.marshalling_nanos),
        ("wave text", profile.wave_nanos),
    ] {
        let _ = writeln!(
            out,
            "  {label:<12} {:>12}  {}",
            duration(nanos),
            percent(nanos, profile.wall_nanos)
        );
    }
    let _ = writeln!(out, "  {:<12} {:>12}", "wall", duration(profile.wall_nanos));

    // The boundary counters, from the same vantage point: measured by the host,
    // not reported by the guest. Peak memory in particular is the number that
    // says whether a plugin is holding on to what crosses into it.
    let _ = writeln!(
        out,
        "  {:<12} {:>12}  ({} call(s), {} fuel)",
        "peak memory",
        bytes(stats.peak_memory_bytes),
        stats.calls,
        stats.fuel_consumed
    );
    // Said every time rather than in the help, because the number is the one a
    // reader is most likely to over-interpret.
    let _ = writeln!(
        out,
        "\nmarshalling is the remainder — the canonical ABI's lift and lower, \
         plus watoots'\nown dispatch. It is a diagnostic, not a measurement."
    );

    if profile.functions.is_empty() {
        return out;
    }

    let _ = writeln!(out, "\nper function");
    for row in &profile.functions {
        let _ = writeln!(out, "  {}", render_row(row));
    }
    // The rows are a subset of the bucket by construction, and saying so beats
    // leaving someone to work out why the arithmetic does not close.
    if profile
        .functions
        .iter()
        .any(|row| row.kind == FunctionKind::Import)
        || profile.host_nanos > 0
    {
        let _ = writeln!(
            out,
            "\nonly host functions this application serves get a row; time in \
             `wasi:` interfaces\nis in the host-call bucket and not in the list."
        );
    }
    out
}

fn render_row(row: &FunctionProfile) -> String {
    let name = if row.interface.is_empty() {
        row.func.clone()
    } else {
        format!("{}#{}", row.interface, row.func)
    };
    match row.kind {
        FunctionKind::Export => format!(
            "export  {name:<44} {:>6} call(s)  wall {:>12}  guest {:>12}  host {:>12}  wave {:>12}",
            row.calls,
            duration(row.wall_nanos),
            duration(row.guest_nanos),
            duration(row.host_nanos),
            duration(row.wave_nanos)
        ),
        FunctionKind::Import => format!(
            "import  {name:<44} {:>6} call(s)  host {:>12}",
            row.calls,
            duration(row.host_nanos)
        ),
    }
}
