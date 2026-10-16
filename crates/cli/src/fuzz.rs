//! `watoots fuzz`: the campaign form of the property tests.
//!
//! The oracles are the ones in `docs/adr/0008-fuzzing.md`, and the generator is
//! literally the same code -- `watoots::fuzz` -- so a finding here and a
//! finding in `cargo test` mean the same thing.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use clap::Args;
use watoots::fuzz::Generator;
use watoots::{Host, ImportedFunction, Manifest, TraceHook, Type, Val};
use watoots_trace::{Header, Recorder, Trace, binary, text};

use crate::{Failure, Outcome, plugin_name, read, read_manifest, write};

#[derive(Args)]
pub(crate) struct FuzzArgs {
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
    let report =
        watoots_trace::replay(trace, wasm).map_err(|err| format!("replay refused: {err}"))?;
    if !report.is_faithful() {
        return Err(report.describe());
    }
    Ok(())
}

/// Property 1, in the form a campaign can apply to a live value.
///
/// Stated as "parsing is a fixed point" rather than as `from_wave(to_wave(v))
/// == v`, because `wasm-wave` normalises as it parses -- it returns a flag set
/// sorted alphabetically rather than in the world's declared order -- and that
/// normalisation is not a defect to report on every run. What would be a defect
/// is a second trip changing the value again, or either trip failing.
fn wave_is_stable(ty: &Type, value: &Val) -> Result<(), String> {
    let render = |value: &Val| {
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

/// The first reason the generator could not build what the component asked
/// for, remembered from wherever it happened.
///
/// A resource handle in an argument or in a host function's result type is not
/// a finding: the component is out of range, and the campaign has to say so and
/// stop rather than file it as a crash.
#[derive(Clone, Default)]
struct Unfuzzable(Arc<Mutex<Option<String>>>);

impl Unfuzzable {
    fn note(&self, err: &watoots::Error) {
        let mut flag = self.0.lock().unwrap_or_else(|err| err.into_inner());
        if flag.is_none() {
            *flag = Some(err.message().to_string());
        }
    }

    fn take(&self) -> Option<String> {
        self.0.lock().unwrap_or_else(|err| err.into_inner()).take()
    }
}

/// One generated session: build a host, call the plugin, take the recording.
struct Session {
    trace: Trace,
    unfuzzable: Option<String>,
    /// A property that failed before the recording was even taken, so the crash
    /// report can carry it alongside the trace.
    finding: Option<String>,
}

/// Everything a session is a function of besides its seed.
struct Campaign<'a> {
    wasm: &'a [u8],
    name: &'a str,
    manifest_toml: &'a str,
    exports: &'a [String],
    /// Every host import the component declares, which the generator answers.
    serve: &'a [ImportedFunction],
    calls: usize,
}

/// Run one session. The generator answers the plugin's imports as well as
/// supplying its arguments, so a whole session is a function of one seed and
/// reproduces exactly.
fn run_session(campaign: &Campaign<'_>, seed: u64) -> Result<Session, Failure> {
    let mut manifest = Manifest::parse(campaign.manifest_toml)?;
    // Same reasoning as `replay`: this runs a component the operator pointed at
    // by name, over and over, to find a crash. The signature policy answers
    // "who published this", which they have already answered by naming the
    // file, and enforcing it here would refuse to fuzz a signed plugin from
    // memory while the permissions and limits that shape the search still
    // apply. See ADR-0014.
    manifest.signature.keys.clear();

    let recorder = Arc::new(Recorder::new(Header {
        component_sha256: Trace::hash_component(campaign.wasm),
        plugin: campaign.name.to_string(),
        manifest_toml: campaign.manifest_toml.to_string(),
    }));

    let generator = Arc::new(Mutex::new(Generator::from_seed(seed)));
    let unfuzzable = Unfuzzable::default();

    let mut builder = Host::builder()
        .manifest(manifest)
        .trace_hook(Arc::clone(&recorder) as Arc<dyn TraceHook>)
        // A granted plugin has to be able to reach somebody, but a campaign
        // that printed every message would bury its own findings.
        .log_sink(|_record| {});

    for imported in campaign.serve {
        let generator = Arc::clone(&generator);
        let unfuzzable = unfuzzable.clone();
        builder = builder.host_func(&imported.interface, &imported.func, move |call| {
            let mut generator = generator.lock().unwrap_or_else(|err| err.into_inner());
            generator
                .values(call.result_types())
                .inspect_err(|err| unfuzzable.note(err))
        });
    }

    let host = builder.build()?;
    let mut plugin = host.load_binary(campaign.name, campaign.wasm)?;

    let mut finding = None;

    for _ in 0..campaign.calls {
        let export = {
            let mut generator = generator.lock().unwrap_or_else(|err| err.into_inner());
            let index = match generator.value(&Type::U8) {
                Ok(Val::U8(byte)) => usize::from(byte),
                _ => 0,
            };
            &campaign.exports[index % campaign.exports.len()]
        };

        let params = plugin.export_params(export)?;

        let args = {
            let mut generator = generator.lock().unwrap_or_else(|err| err.into_inner());
            match generator.values(&params) {
                Ok(args) => args,
                Err(err) => {
                    unfuzzable.note(&err);
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

        if plugin.call(export, &args).is_err() {
            // A trapped component instance refuses re-entry, so anything after
            // this would be recording the engine's refusal rather than the
            // plugin's behaviour. The failure itself is in the trace, and
            // replaying it is part of what is being checked.
            break;
        }
    }

    let mut unfuzzable = unfuzzable.take();

    // A recorder refuses to hand back a trace it could not write down
    // faithfully -- a resource crossing the boundary is the case it exists for.
    // That is the same "out of range" answer, arriving from the other side, and
    // it must not be swallowed into an empty trace that then reports clean.
    let trace = match recorder.finish() {
        Ok(trace) => trace,
        Err(err) => {
            unfuzzable.get_or_insert_with(|| err.message().to_string());
            Trace::default()
        }
    };

    Ok(Session {
        trace,
        unfuzzable,
        finding,
    })
}

pub(crate) fn fuzz(args: &FuzzArgs) -> Outcome {
    let wasm = read(&args.component)?;
    let (_, manifest_toml) = read_manifest(args.manifest.as_deref())?;
    let name = plugin_name(&args.component);
    let shown = args.component.display();

    // Compiles but does not instantiate, so this works before we know whether
    // the manifest is good enough to load the thing.
    let probe = Host::builder().build()?;
    let available = probe.export_functions(&wasm)?;

    let exports: Vec<String> = if args.calls.is_empty() {
        available.clone()
    } else {
        for wanted in &args.calls {
            if !available.contains(wanted) {
                return Err(format!(
                    "{shown} exports no function {wanted:?}; it exports: {}",
                    available.join(", ")
                )
                .into());
            }
        }
        args.calls.clone()
    };
    if exports.is_empty() {
        return Err(format!("{shown} exports no functions to call").into());
    }

    // Everything the component expects the application to provide. WASI is the
    // host library's job, not the mock's -- the same split replay makes.
    let serve: Vec<ImportedFunction> = probe
        .import_functions(&wasm)?
        .into_iter()
        .filter(|imported| !imported.interface.starts_with("wasi:"))
        .collect();

    std::fs::create_dir_all(&args.out)
        .map_err(|err| format!("cannot create {}: {err}", args.out.display()))?;

    eprintln!(
        "fuzzing {shown} — {} case(s) from seed {}, {} call(s) each, over {}",
        args.cases,
        args.seed,
        args.calls_per_case,
        exports.join(", ")
    );

    let campaign = Campaign {
        wasm: &wasm,
        name: &name,
        manifest_toml: &manifest_toml,
        exports: &exports,
        serve: &serve,
        calls: args.calls_per_case,
    };

    let mut crashes = 0usize;
    for case in 0..args.cases {
        let seed = args.seed.wrapping_add(case);
        let session = run_session(&campaign, seed)?;

        if let Some(reason) = session.unfuzzable {
            // Out of range rather than broken, and the same limit that stops
            // the world being traced at all. Say so once and stop.
            return Err(format!("{shown} cannot be fuzzed: {reason}").into());
        }

        if session.trace.events.is_empty() {
            return Err(
                format!("{shown} recorded no crossings; there is nothing here to check").into(),
            );
        }

        let why = match (session.finding, check_session(&session.trace, &wasm)) {
            (Some(why), _) | (None, Err(why)) => why,
            (None, Ok(())) => continue,
        };

        let path = args.out.join(format!("crash-{crashes:03}.wave"));
        write(&path, text::to_text(&session.trace))?;

        println!("crash {crashes} (case {case}, seed {seed}):\n{why}");
        println!("  wrote {}", path.display());
        println!(
            "  reproduce:  watoots fuzz {shown} {}--seed {seed} --cases 1",
            args.manifest
                .as_ref()
                .map_or_else(String::new, |path| format!("-m {} ", path.display()))
        );
        println!(
            "  regression: watoots replay {} --component {shown} --emit-test tests/crash_{crashes:03}.rs",
            path.display()
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
