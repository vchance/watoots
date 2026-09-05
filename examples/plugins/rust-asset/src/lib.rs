//! A sample watoots plugin: an image-operation pipeline, in Rust.
//!
//! Build it with:
//!
//! ```sh
//! cargo build --manifest-path examples/plugins/rust-asset/Cargo.toml \
//!     --target wasm32-wasip2 --release
//! ```
//!
//! Where `rust-lint` shows a plugin that needs no capability at all, this one
//! needs exactly one: `lut` opens a file itself, so `fs.read` has to be granted
//! and scoped, and a manifest that does not grant it fails the *load*.
//!
//! # The arithmetic is a contract
//!
//! Three more guest languages have to reproduce this file byte for byte, and
//! record/replay compares outputs rather than intentions. So every operation
//! below states its exact rule, and the rules are chosen to be the same rule in
//! Rust, C++, JavaScript and Python rather than the prettiest one:
//!
//! | operation   | rule |
//! |-------------|------|
//! | `grayscale` | `y = (299·r + 587·g + 114·b + 500) / 1000`, integer division |
//! | `invert`    | `255 - c` |
//! | `gain`      | `min(255, floor(c · factor + 0.5))` in `f64` |
//! | `resize`    | `sx = dx · src_w / dst_w`, integer division; likewise `sy` |
//! | `lut`       | `out.c = table[in.c].c`, per channel, never across channels |
//!
//! Integer arithmetic wherever it will do the job. The one place a float is
//! unavoidable — `gain`, whose factor is an `f32` in the WIT — widens to `f64`
//! first, which is exact, and rounds with `floor(x + 0.5)`, which is one IEEE
//! operation and a truncation rather than a language's idea of "round".
//!
//! Nothing here reads a clock or asks for randomness. The same input produces
//! the same bytes on every run, on every host, in every language.

wit_bindgen::generate!({
    path: "../../wit/asset",
    world: "asset-plugin",
});

use crate::watoots::asset::log;
use crate::watoots::asset::log::Level;
use crate::watoots::asset::types::{Channel, Extent, FileFailure, Gain, OperationKind};

// `Image`, `Operation`, `Failure` and `PluginInfo` arrive at the root already,
// via the world's own `use`; importing them again would be a duplicate.

/// Bytes per pixel. RGB8, no alpha — the WIT says so.
const CHANNELS: u64 = 3;

/// Ceiling on a `resize` destination, in bytes. **32 MiB, from the WIT.**
///
/// Not a local judgement call: `operation.resize` in `asset.wit` states the
/// bound, so this constant is a transcription of the contract and the other
/// guests must transcribe the same number. Changing it here would be a silent
/// disagreement with the world rather than a tuning decision.
///
/// The reason the world states it at all: a `resize` to 60000×60000 is
/// arithmetically fine and would ask for 10 GB. Under a manifest's memory limit
/// that allocation fails, and a failed allocation in a Rust guest is an abort,
/// which reaches the host as a trap — indistinguishable from a bug. Refusing it
/// as `malformed` keeps "I cannot do this" an answer rather than a crash.
const MAX_IMAGE_BYTES: u64 = 32 * 1024 * 1024;

/// Ceiling on the size of a lookup-table file this plugin will read.
///
/// 256 entries of `255 255 255` is under 3 KB, and `luts/sepia.lut` is 3.5 KB
/// with its header. 64 KiB is room for a generously commented table and a
/// refusal to read anything that is plainly not one. See [`read_capped`].
const MAX_LUT_BYTES: u64 = 64 * 1024;

struct RustAsset;

impl Guest for RustAsset {
    fn describe() -> PluginInfo {
        // `supports` is `list<operation-kind>`, so naming a step no longer
        // means constructing one. There is nothing here for a host to know to
        // ignore, and nothing for four guests to invent differently.
        PluginInfo {
            name: "rust-asset".to_string(),
            supports: vec![
                OperationKind::Grayscale,
                OperationKind::Invert,
                OperationKind::Gain,
                OperationKind::Resize,
                OperationKind::Lut,
            ],
        }
    }

    fn apply(input: Image, steps: Vec<Operation>) -> Result<Image, Failure> {
        // Calling into the host: an import crossing, and one of the things
        // M4's recorder captures. Deterministic on purpose — no counts that
        // depend on anything but the arguments.
        log::emit(
            Level::Info,
            &format!(
                "apply: {}x{}, {} step(s)",
                input.width,
                input.height,
                steps.len()
            ),
        );

        let mut image = validate(input).map_err(report)?;

        // "Steps apply in order. The whole call fails on the first step that
        // cannot be done" — so this is a plain loop with `?`, and a partly
        // transformed image never escapes.
        for step in &steps {
            image = run_step(image, step).map_err(report)?;
        }

        Ok(image)
    }
}

/// Log a failure on its way out, then hand it back unchanged.
///
/// Every case of `failure` now carries its own reason, so **the log line is not
/// how a caller learns why** — it is a courtesy to whoever is watching the
/// host's log, and there is exactly one of it per failed call. Earlier this
/// function was load-bearing, because `unreadable` carried only a path and the
/// reason had nowhere else to go; `file-failure` took that job away from it,
/// which is why the detail below is a rendering of the returned value rather
/// than information the returned value lacks.
fn report(failure: Failure) -> Failure {
    let detail = match &failure {
        // `operation-kind` is an enum now, not a string, so there is a name to
        // print rather than one to invent. Spelled to match the WIT case names,
        // since that is what a reader of the world will be looking for.
        Failure::Unsupported(kind) => {
            let name = match kind {
                OperationKind::Grayscale => "grayscale",
                OperationKind::Invert => "invert",
                OperationKind::Gain => "gain",
                OperationKind::Resize => "resize",
                OperationKind::Lut => "lut",
            };
            format!("unsupported step: {name}")
        }
        Failure::Malformed(why) => format!("malformed input: {why}"),
        Failure::Unreadable(file) => {
            format!("unreadable lookup table {}: {}", file.path, file.reason)
        }
    };
    log::emit(Level::Error, &detail);
    failure
}

/// Check that an image is an image before touching a byte of it.
///
/// A pixel buffer whose length disagrees with the dimensions is exactly the
/// input that turns an indexing bug into a panic, and a panic in a guest is a
/// trap. This is the plugin getting its own house in order: untrusted input
/// arrives here too.
fn validate(image: Image) -> Result<Image, Failure> {
    let expected = u64::from(image.width) * u64::from(image.height) * CHANNELS;
    let actual = image.pixels.len() as u64;
    if actual != expected {
        return Err(Failure::Malformed(format!(
            "{}x{} is {expected} bytes of RGB8, got {actual}",
            image.width, image.height
        )));
    }
    Ok(image)
}

fn run_step(image: Image, step: &Operation) -> Result<Image, Failure> {
    match step {
        Operation::Grayscale => Ok(grayscale(image)),
        Operation::Invert => Ok(invert(image)),
        Operation::Gain(gain) => Ok(apply_gain(image, gain)),
        Operation::Resize(extent) => resize(image, extent),
        Operation::Lut(path) => lut(image, path),
    }
}

/// Rec. 601 luma, replicated across all three channels.
///
/// # Rounding
///
/// ```text
/// y = (299·r + 587·g + 114·b + 500) / 1000
/// ```
///
/// with **truncating integer division**, in `u32`. The coefficients are the
/// Rec. 601 weights scaled by 1000 and the `+ 500` is half the divisor, so the
/// division rounds half **up**; every term is non-negative, so "half up" and
/// "half away from zero" are the same rule and no language has to be asked
/// which one it means. The largest numerator is `255·1000 + 500 = 255500`,
/// well inside `u32`, so this cannot overflow and needs no wider type.
///
/// Deliberately not floating point. `0.299f * r + 0.587f * g + 0.114f * b`
/// gives four different answers in four languages for a handful of inputs,
/// and finding out which handful is not how anyone should spend a week.
fn grayscale(mut image: Image) -> Image {
    for pixel in image.pixels.chunks_exact_mut(3) {
        let r = u32::from(pixel[0]);
        let g = u32::from(pixel[1]);
        let b = u32::from(pixel[2]);
        let luma = ((299 * r + 587 * g + 114 * b + 500) / 1000) as u8;
        pixel[0] = luma;
        pixel[1] = luma;
        pixel[2] = luma;
    }
    image
}

/// `255 - c`, per channel. No rounding: the operation is exact in `u8`.
fn invert(mut image: Image) -> Image {
    for byte in &mut image.pixels {
        *byte = 255 - *byte;
    }
    image
}

/// Multiply one channel by a factor and clamp to `0..=255`.
///
/// # Rounding
///
/// ```text
/// out = min(255, floor(c · factor + 0.5))
/// ```
///
/// computed in `f64`. Three things make that reproducible:
///
/// 1. The factor is an `f32` in the WIT, and widening `f32` to `f64` is exact
///    in every language, so all four start from bit-identical operands. The
///    multiply is then a single IEEE-754 operation, which is correctly rounded
///    and therefore identical everywhere. JavaScript has no `f32` arithmetic
///    at all, so doing the multiply in `f64` is not a compromise for it — it is
///    the only way JavaScript can join in.
/// 2. `floor(x + 0.5)` is spelled the same way and means the same thing in all
///    four. `round()` does not: Rust rounds half away from zero, JavaScript
///    rounds half toward `+∞`, and Python rounds half to even. The inputs here
///    are non-negative, so the rule is round-half-up.
/// 3. The factor is clamped to `0.0..=4.0` and NaN is treated as `0.0`. That is
///    not a local precaution: `gain.factor` in `asset.wit` says the *guest*
///    does this, so it is the contract, and every other guest owes the same two
///    lines. Nothing in the type system makes a host clamp, NaN is
///    representable, and the wrong answer differs by language — Rust's `as u8`
///    saturates to 0, C++'s conversion is undefined behaviour.
///
/// `min(255.0)` before the cast, not `as u8` alone: the product can reach
/// `255 · 4 = 1020`, and a wrapping cast would turn a bright pixel dark.
fn apply_gain(mut image: Image, gain: &Gain) -> Image {
    let factor = if gain.factor.is_nan() {
        0.0
    } else {
        f64::from(gain.factor.clamp(0.0, 4.0))
    };
    let offset = match gain.channel {
        Channel::Red => 0,
        Channel::Green => 1,
        Channel::Blue => 2,
    };

    for pixel in image.pixels.chunks_exact_mut(3) {
        let scaled = (f64::from(pixel[offset]) * factor + 0.5).floor();
        pixel[offset] = scaled.min(255.0) as u8;
    }
    image
}

/// Nearest neighbour, top-left biased. Every rule below is `operation.resize`
/// in `asset.wit`; this function implements the world rather than deciding it.
///
/// # The mapping, exactly
///
/// For destination pixel `(dx, dy)`, with `0 <= dx < dst.width` and
/// `0 <= dy < dst.height`:
///
/// ```text
/// sx = dx · src.width  / dst.width
/// sy = dy · src.height / dst.height
/// ```
///
/// with **truncating integer division**, computed in `u64` so the multiply
/// cannot overflow for any dimensions a `u32` can express. The destination
/// pixel is then a byte-for-byte copy of source pixel `(sx, sy)`.
///
/// There is no `+ 0.5` pixel-centre correction. The usual
/// `floor((dx + 0.5) · src / dst)` is a better resampler and a worse contract:
/// it needs a float or a doubled fixed-point form, and it puts a rounding
/// decision in the hot loop of four independent implementations. The WIT
/// already says this filter is ugly on purpose. `sx < src.width` follows from
/// `dx < dst.width` directly, so the indexing needs no clamp to be safe.
///
/// # The three stated edges
///
/// A destination over [`MAX_IMAGE_BYTES`] is `malformed`; a zero-width or
/// zero-height destination is a zero-pixel image, not an error; and sampling a
/// zero-area *source* into a destination with area is `malformed`, because
/// there is no pixel to copy — and, less politely, because `dst.width / 0` is a
/// trap. All three are in the WIT, so all four guests answer them the same way
/// instead of each discovering them under a different fuzzer.
fn resize(image: Image, extent: &Extent) -> Result<Image, Failure> {
    let (src_w, src_h) = (u64::from(image.width), u64::from(image.height));
    let (dst_w, dst_h) = (u64::from(extent.width), u64::from(extent.height));

    if dst_w == 0 || dst_h == 0 {
        return Ok(Image {
            width: extent.width,
            height: extent.height,
            pixels: Vec::new(),
        });
    }
    if src_w == 0 || src_h == 0 {
        return Err(Failure::Malformed(format!(
            "cannot resize {src_w}x{src_h} to {dst_w}x{dst_h}: no source pixels to sample"
        )));
    }

    let bytes = dst_w * dst_h * CHANNELS;
    if bytes > MAX_IMAGE_BYTES {
        return Err(Failure::Malformed(format!(
            "resize to {dst_w}x{dst_h} would need {bytes} bytes, over the {MAX_IMAGE_BYTES}-byte ceiling"
        )));
    }

    let mut pixels = Vec::with_capacity(bytes as usize);
    for dy in 0..dst_h {
        let sy = dy * src_h / dst_h;
        let row = (sy * src_w * CHANNELS) as usize;
        for dx in 0..dst_w {
            let sx = dx * src_w / dst_w;
            let at = row + (sx * CHANNELS) as usize;
            pixels.extend_from_slice(&image.pixels[at..at + 3]);
        }
    }

    Ok(Image {
        width: extent.width,
        height: extent.height,
        pixels,
    })
}

/// Apply a colour lookup table read from a file.
///
/// This is the step with a capability cost. The plugin opens the file itself,
/// so the host has to grant `fs.read` over a directory the file is in, and the
/// grant is checked against the component's declared imports at *load* time —
/// before a pixel is touched, and before this function has ever run.
///
/// # The argument is a path, spelled the way the grant spells it
///
/// That is the WIT's rule, not this plugin's convention: `operation.lut` in
/// `asset.wit` says so, and says why. A guest has no way to resolve a bare
/// name — the preopen list exists in `wasi:filesystem/preopens`, but this world
/// does not import it and `std::fs` gives no way to ask — and watoots preopens
/// each granted directory under the same string it was granted as. So a grant
/// that expanded to `/abs/plugin/luts` wants `/abs/plugin/luts/sepia.lut`, and
/// one that stayed relative wants a relative argument. This function opens the
/// string as given and nothing more; there is no resolution step to get wrong.
fn lut(image: Image, path: &str) -> Result<Image, Failure> {
    let table = load_lut(path).map_err(|reason| {
        Failure::Unreadable(FileFailure {
            path: path.to_string(),
            reason,
        })
    })?;

    let mut image = image;
    for pixel in image.pixels.chunks_exact_mut(3) {
        // Per channel, never across channels: red is looked up in the red
        // column of the entry the red input selects. Cross-channel mixing
        // would make this a matrix, and the file format is not one.
        pixel[0] = table[pixel[0] as usize][0];
        pixel[1] = table[pixel[1] as usize][1];
        pixel[2] = table[pixel[2] as usize][2];
    }
    Ok(image)
}

/// Read and parse a LUT file.
///
/// # The format
///
/// A text file of 256 entries, one per line, in input order: entry `i` is the
/// output for an input channel value of `i`. Each entry is three decimal
/// integers in `0..=255`, separated by ASCII whitespace, in red green blue
/// order. Blank lines and lines whose first non-space character is `#` are
/// ignored. Anything else must parse, and there must be exactly 256 entries —
/// a short table would silently clip highlights rather than fail.
///
/// Text rather than binary, because the point of the sample is the capability,
/// not the parser, and because a reviewer can read a diff of it.
/// `luts/sepia.lut` in this directory is a worked example.
///
/// # The error type is a reason, and the caller gets it
///
/// `unreadable` carries `file-failure { path, reason }`, so all three causes —
/// not found, not permitted, not a lookup table — arrive at the caller
/// distinguishable without a log sink. The `Err` here is that reason; [`lut`]
/// pairs it with the path, because the path is the argument it was given and
/// this function should not be the one to remember it.
///
/// Reporting a bad table as `malformed` would still be wrong, incidentally:
/// that case is documented as being about the *image*, and would send a host to
/// look at its pixels.
fn load_lut(path: &str) -> Result<Vec<[u8; 3]>, String> {
    let text = read_capped(path).map_err(describe_open_failure)?;

    let mut table = Vec::with_capacity(256);
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if table.len() == 256 {
            return Err(format!(
                "more than 256 entries: a 257th appears on line {}",
                index + 1
            ));
        }
        let entry = parse_entry(line).ok_or_else(|| {
            format!(
                "line {} is not three integers in 0..=255: {line:?}",
                index + 1
            )
        })?;
        table.push(entry);
    }

    if table.len() != 256 {
        return Err(format!("expected 256 entries, found {}", table.len()));
    }
    Ok(table)
}

/// Turn a failed open into a `file-failure.reason`.
///
/// `unreadable`'s doc names three causes — not found, not permitted, not a
/// lookup table — but a WASI guest can only tell the third one apart. A path
/// outside every preopened directory is reported as **not found**, identically
/// to a path inside one that does not exist: the sandbox does not leak the
/// existence of what it is not showing you, which is correct of it and leaves
/// the guest unable to say "not permitted" honestly.
///
/// So it does not guess. It says what it saw and names the thing that is
/// usually actually wrong, which is what the WIT means by "the manifest is
/// usually the answer".
fn describe_open_failure(err: std::io::Error) -> String {
    if err.kind() == std::io::ErrorKind::NotFound {
        return format!(
            "cannot open it: {err} \
             (a path outside every granted directory reports as not found, \
             so check the manifest and the spelling of the path)"
        );
    }
    format!("cannot open it: {err}")
}

/// Read a file, refusing to read more than [`MAX_LUT_BYTES`] of it.
///
/// A grant is a directory, not a file: anything under `luts/` is reachable, and
/// "reachable" includes whatever someone drops there later. `read_to_string`
/// on a 2 GB file would exhaust the manifest's memory limit and abort, which
/// reaches the host as a trap rather than as an answer — the same problem
/// `MAX_IMAGE_BYTES` solves for `resize`. A file that hits the cap gets
/// truncated and then fails the 256-entry check, which is the right answer for
/// a file that large in any case.
fn read_capped(path: &str) -> std::io::Result<String> {
    use std::io::Read as _;

    let mut text = String::new();
    std::fs::File::open(path)?
        .take(MAX_LUT_BYTES)
        .read_to_string(&mut text)?;
    Ok(text)
}

/// One `r g b` line, or `None` if it is not one.
///
/// `u8::from_str` rejects anything out of `0..=255`, a sign, or a stray
/// character, so the range check is the parse.
fn parse_entry(line: &str) -> Option<[u8; 3]> {
    let mut fields = line.split_ascii_whitespace();
    let entry = [
        fields.next()?.parse().ok()?,
        fields.next()?.parse().ok()?,
        fields.next()?.parse().ok()?,
    ];
    if fields.next().is_some() {
        return None;
    }
    Some(entry)
}

export!(RustAsset);
