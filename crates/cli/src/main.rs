//! The `watoots` command line tool.
//!
//! Inspect what a plugin would be granted, run one, record a session, and
//! replay a recording in CI.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use clap::{Args, Parser, Subcommand};
use watoots::fuzz::Generator;
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
    /// Call a plugin with generated arguments and check it records and replays.
    Fuzz(FuzzArgs),
    /// Convert a trace between the text and binary encodings.
    #[command(subcommand)]
    Trace(TraceCommand),
    /// Compare two WIT packages for compatibility.
    #[command(subcommand)]
    Wit(WitCommand),
}

#[derive(Subcommand)]
enum WitCommand {
    /// Check that one WIT package is a compatible successor to another.
    ///
    /// The predicate is wasm-tools': `--current` may have MORE imports and
    /// FEWER exports than `--previous`, and every type it keeps must be
    /// structurally identical. For a plugin world that is the useful
    /// direction -- a host may offer plugins more and demand less of them
    /// without breaking the ones already built.
    ///
    /// Takes two packages rather than a component, which is why it is not a
    /// flag on `inspect`. See ADR-0007.
    SemverCheck(SemverCheckArgs),
}

#[derive(Args)]
struct SemverCheckArgs {
    /// The released package.
    #[arg(long, value_name = "WIT")]
    previous: PathBuf,
    /// The package proposed to replace it.
    #[arg(long, value_name = "WIT")]
    current: PathBuf,
    /// Which world to compare. Optional when each package declares exactly one.
    #[arg(long, value_name = "NAME")]
    world: Option<String>,
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
    /// List every import and its decision instead of summarising capabilities.
    #[arg(long)]
    imports: bool,
    /// Also check the component implements this world. A WIT file, a directory
    /// containing one, or a wasm-encoded WIT package.
    #[arg(long, value_name = "WIT")]
    targets: Option<PathBuf>,
    /// Which world in `--targets` to check against. Optional when the package
    /// declares exactly one.
    #[arg(long, value_name = "NAME", requires = "targets")]
    world: Option<String>,
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

/// `watoots fuzz`: the campaign form of the property tests.
///
/// The oracles are the ones in `docs/adr/0008-fuzzing.md`, and the generator is
/// literally the same code — `watoots::fuzz` — so a finding here and a finding
/// in `cargo test` mean the same thing.
#[derive(Args)]
struct FuzzArgs {
    /// The component to fuzz.
    component: PathBuf,
    /// Manifest granting what the component imports. Without one a component
    /// that imports anything will be refused at load, which is the point of
    /// the manifest and not a finding.
    #[arg(short, long)]
    manifest: Option<PathBuf>,
    /// Exported function to call. Repeat to narrow; the default is all of them.
    #[arg(long = "call", value_name = "NAME")]
    calls: Vec<String>,
    /// How many recorded sessions to run.
    #[arg(long, default_value_t = 64, value_name = "N")]
    cases: u64,
    /// Where the values come from. The same seed runs the same campaign, so a
    /// finding reproduces with `--seed <n> --cases 1`.
    #[arg(long, default_value_t = 0, value_name = "N")]
    seed: u64,
    /// Calls per session. More than one is what exercises replay's ordering.
    #[arg(long, default_value_t = 3, value_name = "N")]
    calls_per_case: usize,
    /// Where to write `crash-NNN.wave`.
    #[arg(long, default_value = ".", value_name = "DIR")]
    out: PathBuf,
    /// Stop after this many crashes. `0` runs the whole campaign.
    #[arg(long, default_value_t = 1, value_name = "N")]
    max_crashes: usize,
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
        Command::Fuzz(args) => fuzz(&args),
        Command::Wit(WitCommand::SemverCheck(args)) => semver_check(&args),
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

    if args.imports {
        print!("{}", report.describe());
    } else {
        print!("{}", report.summarize(&host.manifest().permissions));
    }

    // Imports and exports are separate questions, so they get separate lines
    // and a failure in either fails the command.
    let mut ok = report.is_satisfied();
    if report.is_satisfied() {
        println!("\nevery import is granted");
    } else {
        println!(
            "\n{} import(s) are not granted; `--imports` lists them individually",
            report.denied().count()
        );
    }

    if let Some(wit) = &args.targets {
        match host.check_targets(&wasm, wit, args.world.as_deref()) {
            Ok(()) => println!("exports match the world in {}", wit.display()),
            Err(err) => {
                println!("{}", err.message());
                ok = false;
            }
        }
    }

    Ok(if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// `wasm-tools component semver-check`, wrapped rather than reimplemented.
fn semver_check(args: &SemverCheckArgs) -> Result<ExitCode, String> {
    let mut resolve = wit_parser::Resolve::default();
    let mut load = |path: &Path| -> Result<_, String> {
        resolve
            .push_path(path)
            .map(|(package, _)| package)
            .map_err(|err| format!("{}: {err:#}", path.display()))
    };
    let previous = load(&args.previous)?;
    let current = load(&args.current)?;

    let select = |package, label: &str| {
        resolve
            .select_world(&[package], args.world.as_deref())
            .map_err(|err| format!("{label}: {err:#}"))
    };
    let previous_world = select(previous, "--previous")?;
    let current_world = select(current, "--current")?;

    match wit_component::semver_check(resolve, previous_world, current_world) {
        Ok(()) => {
            println!(
                "{} is a compatible successor to {}",
                args.current.display(),
                args.previous.display()
            );
            Ok(ExitCode::SUCCESS)
        }
        Err(err) => {
            println!("incompatible: {err:#}");
            Ok(ExitCode::FAILURE)
        }
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

/// What one generated session was checked against, and what it found.
///
/// Every check here is one of ADR-0008's properties, applied to a component the
/// user chose rather than to one a test constructed. None of them needs an
/// opinion about what the plugin ought to return: they compare watoots against
/// watoots.
fn check_session(trace: &Trace, wasm: &[u8]) -> Result<(), String> {
    let via_text = text::from_text(&text::to_text(trace))
        .map_err(|err| format!("the trace does not survive its own text encoding: {err}"))?;
    if via_text != *trace {
        return Err("the trace does not survive its own text encoding".to_string());
    }

    let via_binary = binary::from_bytes(&binary::to_bytes(trace))
        .map_err(|err| format!("the trace does not survive its own binary encoding: {err}"))?;
    if via_binary != *trace {
        return Err("the trace does not survive its own binary encoding".to_string());
    }

    // The one that matters: with the application gone, the recording has to
    // reproduce itself.
    let report = replay(trace, wasm).map_err(|err| format!("replay refused: {err}"))?;
    if !report.is_faithful() {
        return Err(report.describe());
    }
    Ok(())
}

/// Property 1, in the form a campaign can apply to a live value.
///
/// Stated as "parsing is a fixed point" rather than as `from_wave(to_wave(v))
/// == v`, because `wasm-wave` normalises as it parses — it returns a flag set
/// sorted alphabetically rather than in the world's declared order — and that
/// normalisation is not a defect to report on every run. What would be a defect
/// is a second trip changing the value again, or either trip failing.
fn wave_is_stable(ty: &watoots::Type, value: &watoots::Val) -> Result<(), String> {
    let render = |value: &watoots::Val| {
        watoots::to_wave(value).map_err(|err| format!("no WAVE rendering: {}", err.message()))
    };
    let parse = |text: &str| {
        watoots::from_wave(ty, text)
            .map_err(|err| format!("{text:?} does not parse back: {}", err.message()))
    };

    let once = parse(&render(value)?)?;
    let twice = parse(&render(&once)?)?;
    if once != twice {
        return Err(format!(
            "a WAVE round trip is not stable:\n  after one: {once:?}\n  after two: {twice:?}"
        ));
    }
    Ok(())
}

/// One generated session: build a host, call the plugin, take the recording.
///
/// The generator answers the plugin's imports as well as supplying its
/// arguments, so a whole session is a function of one seed and reproduces
/// exactly.
struct Session {
    trace: Trace,
    /// Set if the generator was asked for something it cannot build — a
    /// resource handle in an argument or in a host function's result type. Not
    /// a finding: the component is out of range, and the campaign has to say so
    /// and stop rather than file it as a crash.
    unfuzzable: Option<String>,
    /// A property that failed before the recording was even taken, so the crash
    /// report can carry it alongside the trace.
    finding: Option<String>,
}

fn run_session(
    wasm: &[u8],
    name: &str,
    manifest_toml: &str,
    exports: &[String],
    serve: &[watoots::ImportedFunction],
    seed: u64,
    calls: usize,
) -> Result<Session, String> {
    let manifest = Manifest::parse(manifest_toml).map_err(|err| err.message().to_string())?;

    let recorder = Arc::new(Recorder::new(Header {
        component_sha256: Trace::hash_component(wasm),
        plugin: name.to_string(),
        manifest_toml: manifest_toml.to_string(),
    }));

    let generator = Arc::new(Mutex::new(Generator::from_seed(seed)));
    let unfuzzable: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    let mut builder = Host::builder()
        .manifest(manifest)
        .trace_hook(Arc::clone(&recorder) as Arc<dyn TraceHook>)
        // A granted plugin has to be able to reach somebody, but a campaign
        // that printed every message would bury its own findings.
        .log_sink(|_record| {});

    for imported in serve {
        let generator = Arc::clone(&generator);
        let unfuzzable = Arc::clone(&unfuzzable);
        builder = builder.host_func(&imported.interface, &imported.func, move |call| {
            let mut generator = generator.lock().unwrap_or_else(|err| err.into_inner());
            generator.values(call.result_types()).inspect_err(|err| {
                let mut flag = unfuzzable.lock().unwrap_or_else(|err| err.into_inner());
                if flag.is_none() {
                    *flag = Some(err.message().to_string());
                }
            })
        });
    }

    let host = builder.build().map_err(|err| err.message().to_string())?;
    let mut plugin = host
        .load_binary(name, wasm)
        .map_err(|err| err.message().to_string())?;

    let mut finding = None;

    for _ in 0..calls {
        let export = {
            let mut generator = generator.lock().unwrap_or_else(|err| err.into_inner());
            let index = match generator.value(&watoots::Type::U8) {
                Ok(watoots::Val::U8(byte)) => usize::from(byte),
                _ => 0,
            };
            exports[index % exports.len()].clone()
        };

        let params = plugin
            .export_params(&export)
            .map_err(|err| err.message().to_string())?;

        let args = {
            let mut generator = generator.lock().unwrap_or_else(|err| err.into_inner());
            match generator.values(&params) {
                Ok(args) => args,
                Err(err) => {
                    let mut flag = unfuzzable.lock().unwrap_or_else(|err| err.into_inner());
                    if flag.is_none() {
                        *flag = Some(err.message().to_string());
                    }
                    break;
                }
            }
        };

        // Property 1, on this component's own types. A failure here is a
        // finding like any other, so it is remembered rather than raised: the
        // session still has to be recorded, or the crash report would have no
        // trace attached to it.
        for (ty, value) in params.iter().zip(&args) {
            if let Err(why) = wave_is_stable(ty, value)
                && finding.is_none()
            {
                finding = Some(format!("{export}: {why}"));
            }
        }

        if plugin.call(&export, &args).is_err() {
            // A trapped component instance refuses re-entry, so anything after
            // this would be recording the engine's refusal rather than the
            // plugin's behaviour. The failure itself is in the trace, and
            // replaying it is part of what is being checked.
            break;
        }
    }

    let mut unfuzzable = unfuzzable
        .lock()
        .unwrap_or_else(|err| err.into_inner())
        .clone();

    // A recorder refuses to hand back a trace it could not write down
    // faithfully — a resource crossing the boundary is the case it exists for.
    // That is the same "out of range" answer, arriving from the other side, and
    // it must not be swallowed into an empty trace that then reports clean.
    let trace = match recorder.finish() {
        Ok(trace) => trace,
        Err(err) => {
            if unfuzzable.is_none() {
                unfuzzable = Some(err.message().to_string());
            }
            Trace::default()
        }
    };

    Ok(Session {
        trace,
        unfuzzable,
        finding,
    })
}

fn fuzz(args: &FuzzArgs) -> Result<ExitCode, String> {
    let wasm = read(&args.component)?;
    let manifest_toml = match &args.manifest {
        Some(path) => std::fs::read_to_string(path)
            .map_err(|err| format!("cannot read {}: {err}", path.display()))?,
        None => String::new(),
    };
    let name: String = args.component.file_stem().map_or_else(
        || "plugin".to_string(),
        |stem| stem.to_string_lossy().into(),
    );

    // Compiles but does not instantiate, so this works before we know whether
    // the manifest is good enough to load the thing.
    let probe = Host::builder()
        .build()
        .map_err(|err| err.message().to_string())?;
    let available = probe
        .export_functions(&wasm)
        .map_err(|err| err.message().to_string())?;

    let exports: Vec<String> = if args.calls.is_empty() {
        available.clone()
    } else {
        for wanted in &args.calls {
            if !available.contains(wanted) {
                return Err(format!(
                    "{} exports no function {wanted:?}; it exports: {}",
                    args.component.display(),
                    available.join(", ")
                ));
            }
        }
        args.calls.clone()
    };
    if exports.is_empty() {
        return Err(format!(
            "{} exports no functions to call",
            args.component.display()
        ));
    }

    // Everything the component expects the application to provide. WASI is the
    // host library's job, not the mock's — the same split replay makes.
    let serve: Vec<watoots::ImportedFunction> = probe
        .import_functions(&wasm)
        .map_err(|err| err.message().to_string())?
        .into_iter()
        .filter(|imported| !imported.interface.starts_with("wasi:"))
        .collect();

    std::fs::create_dir_all(&args.out)
        .map_err(|err| format!("cannot create {}: {err}", args.out.display()))?;

    eprintln!(
        "fuzzing {} — {} case(s) from seed {}, {} call(s) each, over {}",
        args.component.display(),
        args.cases,
        args.seed,
        args.calls_per_case,
        exports.join(", ")
    );

    let mut crashes = 0usize;
    for case in 0..args.cases {
        let seed = args.seed.wrapping_add(case);
        let session = run_session(
            &wasm,
            &name,
            &manifest_toml,
            &exports,
            &serve,
            seed,
            args.calls_per_case,
        )?;

        if let Some(reason) = session.unfuzzable {
            // Out of range rather than broken, and the same limit that stops
            // the world being traced at all. Say so once and stop.
            return Err(format!(
                "{} cannot be fuzzed: {reason}",
                args.component.display()
            ));
        }

        if session.trace.events.is_empty() {
            return Err(format!(
                "{} recorded no crossings; there is nothing here to check",
                args.component.display()
            ));
        }

        let why = match (session.finding, check_session(&session.trace, &wasm)) {
            (Some(why), _) | (None, Err(why)) => why,
            (None, Ok(())) => continue,
        };

        let path = args.out.join(format!("crash-{crashes:03}.wave"));
        std::fs::write(&path, text::to_text(&session.trace))
            .map_err(|err| format!("cannot write {}: {err}", path.display()))?;

        println!("crash {crashes} (case {case}, seed {seed}):\n{why}");
        println!("  wrote {}", path.display());
        println!(
            "  reproduce:  watoots fuzz {} {}--seed {seed} --cases 1",
            args.component.display(),
            args.manifest
                .as_ref()
                .map_or_else(String::new, |path| format!("-m {} ", path.display()))
        );
        println!(
            "  regression: watoots replay {} --component {} --emit-test tests/crash_{crashes:03}.rs",
            path.display(),
            args.component.display()
        );

        crashes += 1;
        if args.max_crashes != 0 && crashes >= args.max_crashes {
            eprintln!(
                "stopping after {crashes} crash(es); --max-crashes 0 runs the whole campaign"
            );
            break;
        }
    }

    if crashes == 0 {
        eprintln!("{} case(s), no findings", args.cases);
        return Ok(ExitCode::SUCCESS);
    }
    Ok(ExitCode::FAILURE)
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
