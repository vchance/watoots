//! The trace file: `record`, `replay` and `trace fmt`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Args, Subcommand};
use watoots::TraceHook;
use watoots_trace::{Header, Recorder, Trace, binary, text};

use crate::run::{Invocation, build_host, call_and_print};
use crate::{Failure, Outcome, plugin_name, read, read_manifest, verdict, write};

#[derive(Args)]
pub(crate) struct RecordArgs {
    #[command(flatten)]
    invocation: Invocation,
    /// Where to write the trace. `.wtr` writes the binary encoding.
    #[arg(short, long)]
    output: PathBuf,
}

#[derive(Args)]
pub(crate) struct ReplayArgs {
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
pub(crate) enum TraceCommand {
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

pub(crate) fn record(args: &RecordArgs) -> Outcome {
    let invocation = &args.invocation;
    // Read once, used twice: the bytes that are hashed into the header are the
    // bytes that run, and the manifest the header carries is the one the host
    // was built from. Two reads of either would let a file changed underneath
    // the command produce a trace describing something that did not happen.
    let wasm = read(&invocation.component)?;
    let (manifest, manifest_toml) = read_manifest(invocation.manifest.as_deref())?;
    let name = plugin_name(&invocation.component);

    let recorder = Arc::new(Recorder::new(Header {
        component_sha256: Trace::hash_component(&wasm),
        plugin: name.clone(),
        manifest_toml,
    }));

    let outcome = (|| -> Outcome {
        let host = build_host(
            invocation,
            manifest,
            Some(Arc::clone(&recorder) as Arc<dyn TraceHook>),
            None,
        )?;
        // From the bytes already read, with the signature `Host::load` would
        // have looked for beside the file, so a signed policy records too.
        let mut plugin = match read(&signature_beside(&invocation.component)) {
            Ok(signature) => host.load_binary_signed(&name, &wasm, &signature)?,
            Err(_) => host.load_binary(&name, &wasm)?,
        };
        call_and_print(&mut plugin, invocation)?;
        Ok(ExitCode::SUCCESS)
    })();

    // A call that failed is the recording worth keeping. The trace format
    // carries the failure as an outcome, the host records it, and replay
    // reproduces it -- the first version of this command threw all of that away
    // with a `?`, which meant the one session anyone actually wants a file for
    // was the one it refused to write. Found by the preview demo trying to
    // record a decoder hitting its memory ceiling.
    //
    // A failure *before* the call -- the component would not load, the manifest
    // would not parse -- has no crossings and gets no file: an empty trace for
    // a load error would misdescribe what happened.
    let trace = recorder.finish()?;
    if trace.events.is_empty() {
        return outcome;
    }
    write_trace(&trace, &args.output)?;
    eprintln!(
        "wrote {} ({} crossings)",
        args.output.display(),
        trace.events.len()
    );
    match outcome {
        Ok(code) => Ok(code),
        Err(failure) => {
            eprintln!("watoots: {failure}");
            Ok(ExitCode::FAILURE)
        }
    }
}

/// `<component>.sig`: where `Host::load` reads a signature from.
fn signature_beside(component: &Path) -> PathBuf {
    let mut path = component.as_os_str().to_os_string();
    path.push(".sig");
    PathBuf::from(path)
}

pub(crate) fn replay(args: &ReplayArgs) -> Outcome {
    let trace = load_trace(&args.trace)?;
    let wasm = read(&args.component)?;

    let report = watoots_trace::replay(&trace, &wasm)?;
    println!("{}", report.describe());

    if let Some(path) = &args.emit_test {
        write_test(path, &args.trace, &args.component)?;
        eprintln!("wrote {}", path.display());
    }

    verdict(!args.assert || report.is_faithful())
}

pub(crate) fn trace(command: TraceCommand) -> Outcome {
    let TraceCommand::Fmt {
        input,
        output,
        binary: as_binary,
    } = command;
    let trace = load_trace(&input)?;
    match output {
        Some(path) => write(&path, encode(&trace, as_binary))?,
        None if as_binary => {
            return Err("--binary needs --output; a trace is not printed as bytes".into());
        }
        None => print!("{}", text::to_text(&trace)),
    }
    Ok(ExitCode::SUCCESS)
}

/// Read a trace in either encoding, deciding from its contents.
fn load_trace(path: &Path) -> Result<Trace, Failure> {
    Ok(watoots_trace::decode(&read(path)?)
        .map_err(|err| format!("{}: {}", path.display(), err.message()))?)
}

/// A trace as bytes, in the encoding asked for.
fn encode(trace: &Trace, as_binary: bool) -> Vec<u8> {
    if as_binary {
        binary::to_bytes(trace)
    } else {
        text::to_text(trace).into_bytes()
    }
}

/// Write a trace, choosing the encoding by extension: `.wtr` is binary.
fn write_trace(trace: &Trace, path: &Path) -> Result<(), Failure> {
    let as_binary = path.extension().is_some_and(|ext| ext == "wtr");
    write(path, encode(trace, as_binary))
}

/// Write a Rust test that performs this replay.
///
/// The point of `--emit-test`: a bug report becomes a file, and that file
/// becomes a regression test with no host code around it. The test reads the
/// trace through `watoots_trace::decode`, so it works for either encoding.
fn write_test(path: &Path, trace: &Path, component: &Path) -> Result<(), Failure> {
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
    let bytes = std::fs::read({trace:?}).expect("reading the trace");
    let trace = watoots_trace::decode(&bytes).expect("parsing the trace");

    let report = watoots_trace::replay(&trace, &wasm).expect("replaying");
    assert!(report.is_faithful(), "{{}}", report.describe());
}}
"#
    );
    write(path, source)
}
