//! The `watoots` command line tool.
//!
//! Inspect what a plugin would be granted, run one, record a session, and
//! replay a recording in CI.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Args, Parser, Subcommand};
use watoots::{Host, HostBuilder, Manifest, TraceHook};
use watoots_trace::{Header, Recorder, Trace, binary, replay, text};

#[derive(Parser)]
#[command(name = "watoots", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Explain a component's imports as a permission grant list.
    Inspect(InspectArgs),
    /// Call an exported function.
    Run(RunArgs),
    /// Call an exported function and write a trace of every crossing.
    Record(RecordArgs),
    /// Re-run a recorded session against a component, with no application.
    Replay(ReplayArgs),
    /// Convert a trace between the text and binary encodings.
    #[command(subcommand)]
    Trace(TraceCommand),
}

#[derive(Args)]
struct InspectArgs {
    /// The component to inspect.
    component: PathBuf,
    /// Manifest to check against. Without one, everything is denied, which is
    /// the most useful default here: it shows the full bill.
    #[arg(short, long)]
    manifest: Option<PathBuf>,
    /// An interface the application serves, so it is not reported as a denial.
    /// Repeat for each one.
    #[arg(long = "provide", value_name = "INTERFACE")]
    provide: Vec<String>,
}

/// What `run` and `record` share.
#[derive(Args, Clone)]
struct Invocation {
    /// The component to load.
    component: PathBuf,
    /// Manifest granting what the component imports.
    #[arg(short, long)]
    manifest: Option<PathBuf>,
    /// Exported function to call.
    #[arg(short, long)]
    call: String,
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
struct RunArgs {
    #[command(flatten)]
    invocation: Invocation,
}

#[derive(Args)]
struct RecordArgs {
    #[command(flatten)]
    invocation: Invocation,
    /// Where to write the trace. `.wtr` writes the binary encoding.
    #[arg(short, long)]
    output: PathBuf,
}

#[derive(Args)]
struct ReplayArgs {
    /// The trace to replay.
    trace: PathBuf,
    /// The component it was recorded against.
    #[arg(short, long)]
    component: PathBuf,
    /// Exit non-zero on any divergence. This is the CI switch.
    #[arg(long)]
    assert: bool,
    /// Write a Rust test that performs this replay.
    #[arg(long, value_name = "FILE")]
    emit_test: Option<PathBuf>,
}

#[derive(Subcommand)]
enum TraceCommand {
    /// Convert a trace between encodings.
    Fmt {
        /// The trace to read; either encoding, detected from its contents.
        input: PathBuf,
        /// Where to write. Defaults to stdout for text.
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// Write the binary encoding instead of text.
        #[arg(long)]
        binary: bool,
    },
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("watoots: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode, String> {
    match cli.command {
        Command::Inspect(args) => inspect(&args),
        Command::Run(args) => invoke(&args.invocation, None),
        Command::Record(args) => record(&args),
        Command::Replay(args) => do_replay(&args),
        Command::Trace(TraceCommand::Fmt {
            input,
            output,
            binary: as_binary,
        }) => fmt_trace(&input, output.as_deref(), as_binary),
    }
}

fn read_manifest(path: Option<&Path>) -> Result<Manifest, String> {
    match path {
        None => Ok(Manifest::default()),
        Some(path) => Manifest::from_file(path).map_err(|err| err.message().to_string()),
    }
}

fn inspect(args: &InspectArgs) -> Result<ExitCode, String> {
    let wasm = read(&args.component)?;
    let mut builder = Host::builder().manifest(read_manifest(args.manifest.as_deref())?);
    for interface in &args.provide {
        builder = builder.provide_interface(interface.clone());
    }
    let host = builder.build().map_err(|err| err.message().to_string())?;

    let report = host
        .inspect(&wasm)
        .map_err(|err| err.message().to_string())?;
    print!("{}", report.describe());

    if report.is_satisfied() {
        println!("\nevery import is granted");
        Ok(ExitCode::SUCCESS)
    } else {
        println!("\n{} import(s) are not granted", report.denied().count());
        Ok(ExitCode::FAILURE)
    }
}

/// One `--answer iface#func=wave` flag.
struct Answer {
    interface: String,
    func: String,
    value: Option<String>,
}

fn parse_answers(raw: &[String]) -> Result<Vec<Answer>, String> {
    raw.iter()
        .map(|entry| {
            let (target, value) = entry.split_once('=').ok_or_else(|| {
                format!("--answer needs <interface>#<function>=<wave>: {entry:?}")
            })?;
            let (interface, func) = target.split_once('#').ok_or_else(|| {
                format!("--answer needs <interface>#<function>=<wave>: {entry:?}")
            })?;
            Ok(Answer {
                interface: interface.to_string(),
                func: func.to_string(),
                value: (!value.is_empty()).then(|| value.to_string()),
            })
        })
        .collect()
}

/// Build a host that serves whatever the caller said to answer with.
fn build_host(invocation: &Invocation, hook: Option<Arc<dyn TraceHook>>) -> Result<Host, String> {
    let mut builder: HostBuilder =
        Host::builder().manifest(read_manifest(invocation.manifest.as_deref())?);

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
    builder.build().map_err(|err| err.message().to_string())
}

fn invoke(invocation: &Invocation, hook: Option<Arc<dyn TraceHook>>) -> Result<ExitCode, String> {
    let host = build_host(invocation, hook)?;
    let mut plugin = host
        .load(&invocation.component)
        .map_err(|err| err.message().to_string())?;

    let args: Vec<&str> = invocation.args.iter().map(String::as_str).collect();
    let results = plugin
        .call_wave(&invocation.call, &args)
        .map_err(|err| err.message().to_string())?;

    for value in &results {
        println!("{value}");
    }
    Ok(ExitCode::SUCCESS)
}

fn record(args: &RecordArgs) -> Result<ExitCode, String> {
    let wasm = read(&args.invocation.component)?;
    let manifest_toml = match &args.invocation.manifest {
        Some(path) => std::fs::read_to_string(path)
            .map_err(|err| format!("cannot read {}: {err}", path.display()))?,
        None => String::new(),
    };

    let name = args
        .invocation
        .component
        .file_stem()
        .map_or_else(|| "plugin".to_string(), |s| s.to_string_lossy().into());

    let recorder = Arc::new(Recorder::new(Header {
        component_sha256: Trace::hash_component(&wasm),
        plugin: name,
        manifest_toml,
    }));

    let code = invoke(
        &args.invocation,
        Some(Arc::clone(&recorder) as Arc<dyn TraceHook>),
    )?;

    let trace = recorder.finish().map_err(|err| err.message().to_string())?;
    write_trace(&trace, &args.output)?;
    eprintln!(
        "wrote {} ({} crossings)",
        args.output.display(),
        trace.events.len()
    );
    Ok(code)
}

fn do_replay(args: &ReplayArgs) -> Result<ExitCode, String> {
    let trace = load_trace(&args.trace)?;
    let wasm = read(&args.component)?;

    let report = replay(&trace, &wasm).map_err(|err| err.message().to_string())?;
    println!("{}", report.describe());

    if let Some(path) = &args.emit_test {
        write_test(path, &args.trace, &args.component)?;
        eprintln!("wrote {}", path.display());
    }

    if args.assert && !report.is_faithful() {
        return Ok(ExitCode::FAILURE);
    }
    Ok(ExitCode::SUCCESS)
}

fn fmt_trace(input: &Path, output: Option<&Path>, as_binary: bool) -> Result<ExitCode, String> {
    let trace = load_trace(input)?;
    match output {
        Some(path) => {
            if as_binary {
                std::fs::write(path, binary::to_bytes(&trace))
            } else {
                std::fs::write(path, text::to_text(&trace))
            }
            .map_err(|err| format!("cannot write {}: {err}", path.display()))?;
        }
        None if as_binary => {
            return Err("--binary needs --output; a trace is not printed as bytes".to_string());
        }
        None => print!("{}", text::to_text(&trace)),
    }
    Ok(ExitCode::SUCCESS)
}

/// Read a trace in either encoding, deciding from its contents.
fn load_trace(path: &Path) -> Result<Trace, String> {
    let bytes = read(path)?;
    if bytes.starts_with(b"WTTR") {
        return binary::from_bytes(&bytes).map_err(|err| err.message().to_string());
    }
    let text = String::from_utf8(bytes)
        .map_err(|_| format!("{} is neither a text nor a binary trace", path.display()))?;
    text::from_text(&text).map_err(|err| err.message().to_string())
}

fn write_trace(trace: &Trace, path: &Path) -> Result<(), String> {
    let binary_wanted = path.extension().is_some_and(|ext| ext == "wtr");
    let result = if binary_wanted {
        std::fs::write(path, binary::to_bytes(trace))
    } else {
        std::fs::write(path, text::to_text(trace))
    };
    result.map_err(|err| format!("cannot write {}: {err}", path.display()))
}

/// Write a Rust test that performs this replay.
///
/// The point of `--emit-test`: a bug report becomes a file, and that file
/// becomes a regression test with no host code around it.
fn write_test(path: &Path, trace: &Path, component: &Path) -> Result<(), String> {
    let name = trace
        .file_stem()
        .map_or_else(|| "trace".to_string(), |s| s.to_string_lossy().into())
        .replace(['-', '.', ' '], "_");

    let source = format!(
        r#"// Generated by `watoots replay --emit-test`.
//
// Reproduces a recorded session with no application present: the trace answers
// the plugin's imports and drives its exports.

#[test]
fn replays_{name}() {{
    let wasm = std::fs::read({component:?}).expect("reading the component");
    let text = std::fs::read_to_string({trace:?}).expect("reading the trace");
    let trace = watoots_trace::text::from_text(&text).expect("parsing the trace");

    let report = watoots_trace::replay(&trace, &wasm).expect("replaying");
    assert!(report.is_faithful(), "{{}}", report.describe());
}}
"#
    );
    std::fs::write(path, source).map_err(|err| format!("cannot write {}: {err}", path.display()))
}

fn read(path: &Path) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|err| format!("cannot read {}: {err}", path.display()))
}
