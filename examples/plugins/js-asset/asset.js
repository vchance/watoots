// A sample watoots plugin: an image-operation pipeline, in JavaScript.
//
//   tools/build-plugins.sh js-asset
//
// The same world as `examples/plugins/rust-asset`, and the same *bytes*.
// `examples/wit/asset/asset.wit` says four languages have to agree on every
// output byte; this is the third of them, and the first written in a language
// with no integers, no `f32`, and no way to free anything on purpose.
//
// Where `js-lint` shows a JavaScript guest that needs no capability, this one
// needs exactly one: `lut` opens a file itself. In this guest that costs more
// than a grant -- see `wit/js-asset.wit`, which has to name `wasi:filesystem`
// out loud because StarlingMonkey exposes no file API to JavaScript and a guest
// may only import what its world declares.
//
// # Everything here is a transcription, not a decision
//
// `examples/plugins/rust-asset/src/lib.rs` is the reference implementation and
// documents every arithmetic rule on the function that implements it. This file
// implements the same rules and says so at each site; where JavaScript offers a
// shorter spelling that would round differently, the comment says which one was
// rejected and why. The rules, once more:
//
//   grayscale  y = (299*r + 587*g + 114*b + 500) / 1000, integer division
//   invert     255 - c
//   gain       min(255, floor(c * factor + 0.5)) in f64
//   resize     sx = dx * src_w / dst_w, integer division; likewise sy
//   lut        out.c = table[in.c].c, per channel, never across channels
//
// Two of those want a note in this language specifically:
//
//   * `Math.round` is **not** the trap it looks like, and saying so is more
//     useful than repeating the warning. It is specified as half-toward-
//     +Infinity, which on the non-negative operands `gain` produces is the same
//     answer as `floor(x + 0.5)`; the spec's own note says the two diverge only
//     for `-0.5 <= x < 0` and for an `x` so large that `x + 0.5` is inexact,
//     neither of which is reachable from `0..=255` times `0.0..=4.0`. Rust's
//     `f64::round` -- half-away-from-zero -- coincides for the same reason. The
//     rule the world states is there for **Python**, whose `round` is
//     half-to-even and really does answer differently. Which is exactly why it
//     is stated: a contract that four guests satisfy by coincidence is a
//     contract nobody has checked. `Math.round` still appears nowhere below.
//   * JavaScript has no `f32` arithmetic. That is not a problem here, it is the
//     reason the world states `gain` in `f64`: a `float32` lifted into a JS
//     number *is* the exact widening every other guest performs before it
//     multiplies, so all four start from bit-identical operands.
//
// # Numbers
//
// A JS number is an f64, exact on integers below 2^53. Two places can exceed
// that and both are done in `BigInt`, which is exact without a bound:
// `validate`'s expected length and `resize`'s destination byte count, either of
// which a hostile `u32` pair can push past 2^53. Everything inside the resize
// loop stays in plain numbers, with the bound argued at the site.
//
// Nothing here reads a clock or asks for randomness. The same input produces the
// same bytes on every run.

import { emit } from 'watoots:asset/log@0.1.0';
import { getDirectories } from 'wasi:filesystem/preopens@0.2.10';

/** Bytes per pixel. RGB8, no alpha -- the WIT says so. */
const CHANNELS = 3;

/**
 * Ceiling on a `resize` destination, in bytes. **32 MiB, from the WIT.**
 *
 * Transcribed from `operation.resize` in `asset.wit`, the same way the other
 * guests transcribe it. Changing it here would be a silent disagreement with
 * the world rather than a tuning decision.
 *
 * A `BigInt`, because it is compared against one.
 */
const MAX_IMAGE_BYTES = 33554432n;

/**
 * Ceiling on the size of a lookup-table file this plugin will read.
 *
 * Matches `MAX_LUT_BYTES` in the Rust guest, and for the same reason: a grant is
 * a directory, so "what is in luts/" is not a closed set, and reading all of
 * whatever someone drops there would exhaust the manifest's memory limit. A file
 * that hits the cap is truncated and then fails the 256-entry check.
 */
const MAX_LUT_BYTES = 65536;

/**
 * How a WASI resource handle is released.
 *
 * Spelled with the same fallback jco's own bindings use: `Symbol.dispose` is a
 * recent addition and `Symbol.for('dispose')` is what the generated resource
 * classes fall back to. Writing `Symbol.dispose` alone would silently no-op on
 * an engine without it -- and a no-op here leaks a handle per call rather than
 * failing, which is the kind of bug that only shows up under load.
 */
const symbolDispose = Symbol.dispose ?? Symbol.for('dispose');

/**
 * A failure on its way out of `apply`.
 *
 * jco lowers a thrown value into the `err` arm when it carries an own `payload`
 * property; anything else that is an `Error` is rethrown and becomes a trap. So
 * the class exists to make "I cannot do this" an answer rather than a crash,
 * which is exactly the distinction `result<image, failure>` is in the WIT for.
 */
class Refusal extends Error {
  constructor(payload) {
    super();
    this.payload = payload;
  }
}

const malformed = (reason) => new Refusal({ tag: 'malformed', val: reason });

const unreadable = (path, reason) =>
  new Refusal({ tag: 'unreadable', val: { path, reason } });

/**
 * Log a failure on its way out, then hand it back to be thrown.
 *
 * The mirror of `report` in the Rust guest. Every case of `failure` carries its
 * own reason, so this line is a courtesy to whoever is watching the host's log
 * rather than the only channel, and there is exactly one of it per failed call.
 */
function report(refusal) {
  const failure = refusal.payload;
  let detail;
  switch (failure.tag) {
    case 'unsupported':
      // Unreachable today -- this plugin implements every case -- and kept so
      // the guests stay line-for-line comparable. `operation-kind` lowers to
      // the WIT case name already, so there is nothing to spell out.
      detail = `unsupported step: ${failure.val}`;
      break;
    case 'malformed':
      detail = `malformed input: ${failure.val}`;
      break;
    default:
      detail = `unreadable lookup table ${failure.val.path}: ${failure.val.reason}`;
      break;
  }
  emit('error', detail);
  return refusal;
}

// ---------------------------------------------------------------------------
// The operations
// ---------------------------------------------------------------------------

/**
 * Check that an image is an image before touching a byte of it.
 *
 * A pixel buffer whose length disagrees with the dimensions is exactly the input
 * that turns an indexing bug into garbage. Untrusted input arrives here too.
 *
 * `BigInt` and not a number: `width` and `height` are `u32`, so the product can
 * reach 5.5e19 and an f64 stops being exact at 9.0e15. The comparison would
 * still usually be right; the number in the message would not.
 */
function validate(image) {
  const expected = BigInt(image.width) * BigInt(image.height) * BigInt(CHANNELS);
  const actual = BigInt(image.pixels.length);
  if (actual !== expected) {
    throw malformed(
      `${image.width}x${image.height} is ${expected} bytes of RGB8, got ${actual}`,
    );
  }
}

/**
 * Rec. 601 luma, replicated across all three channels.
 *
 *   y = (299*r + 587*g + 114*b + 500) / 1000
 *
 * with truncating integer division. Matching the Rust guest, which explains the
 * choice at length: the coefficients are the Rec. 601 weights scaled by 1000,
 * the `+ 500` is half the divisor so the division rounds half up, and every term
 * is non-negative so "half up" and "half away from zero" are the same rule.
 *
 * JavaScript has no integer division, so this is an f64 divide and a truncation.
 * That is exact here rather than approximately exact: the numerator is at most
 * 255500, IEEE division is correctly rounded, and the true quotient is never
 * closer than 1/1000 to an integer it is not equal to -- eleven orders of
 * magnitude more than an ulp at this scale. `Math.trunc` cannot land on the
 * wrong side of a boundary.
 *
 * Deliberately not `0.299 * r + ...`. That is the spelling JavaScript makes
 * easiest and it is the one that disagrees with the other three guests.
 */
function grayscale(image) {
  const pixels = image.pixels;
  for (let at = 0; at + 3 <= pixels.length; at += 3) {
    const luma = Math.trunc(
      (299 * pixels[at] + 587 * pixels[at + 1] + 114 * pixels[at + 2] + 500) /
        1000,
    );
    pixels[at] = luma;
    pixels[at + 1] = luma;
    pixels[at + 2] = luma;
  }
}

/** `255 - c`, per channel. No rounding: the operation is exact in 8 bits. */
function invert(image) {
  const pixels = image.pixels;
  for (let at = 0; at < pixels.length; at += 1) {
    pixels[at] = 255 - pixels[at];
  }
}

/**
 * Multiply one channel by a factor and clamp to 0..=255.
 *
 *   out = min(255, floor(c * factor + 0.5))
 *
 * in f64, matching the Rust guest exactly. Three things make that reproducible,
 * and this guest is the one the second of them was written for:
 *
 *  1. The factor is an `f32` in the WIT. jco lifts it into a JS number, which is
 *     an f64 holding exactly the widened `f32` -- the same widening
 *     `f64::from(f32)` and `static_cast<double>(float)` perform. The multiply is
 *     then one IEEE-754 operation, correctly rounded, identical everywhere.
 *  2. `Math.floor(x + 0.5)`, and not `Math.round` -- though `Math.round` would
 *     in fact pass. It is half-toward-+Infinity, which is the same answer as
 *     `floor(x + 0.5)` for every non-negative operand this function sees, and
 *     Rust's half-away-from-zero coincides too. Python's `round` is
 *     half-to-even and does not. So writing the rule out is what turns "three
 *     guests happen to agree" into "four guests were told what to do" -- and
 *     the coincidence is not one to lean on: the ECMAScript spec notes that
 *     `Math.round(x)` and `Math.floor(x + 0.5)` also part company once `x` is
 *     large enough for `x + 0.5` to be inexact.
 *  3. The factor is clamped to 0.0..=4.0 and NaN is treated as 0.0. That is the
 *     WIT's rule, stated on `gain.factor`. `Math.max(0, NaN)` is `NaN`, so the
 *     NaN test comes first and is not folded into the clamp -- in this language
 *     an unclamped NaN would not saturate or invoke undefined behaviour, it
 *     would write `NaN` into a `Uint8Array`, which stores 0 and looks like a
 *     black pixel someone meant.
 *
 * `Math.min(255, ...)` before the store, not the store alone: the product can
 * reach 255 * 4 = 1020, and writing that to a `Uint8Array` keeps the low eight
 * bits -- 1020 would land as 252 and turn a white pixel grey.
 */
function applyGain(image, gain) {
  const factor = Number.isNaN(gain.factor)
    ? 0
    : Math.min(4, Math.max(0, gain.factor));
  const offset = { red: 0, green: 1, blue: 2 }[gain.channel];

  const pixels = image.pixels;
  for (let at = 0; at + 3 <= pixels.length; at += 3) {
    const scaled = Math.floor(pixels[at + offset] * factor + 0.5);
    pixels[at + offset] = Math.min(255, scaled);
  }
}

/**
 * Nearest neighbour, top-left biased. Every rule is `operation.resize` in
 * `asset.wit`; this implements the world rather than deciding it.
 *
 *   sx = dx * src_w / dst_w
 *   sy = dy * src_h / dst_h
 *
 * with truncating integer division. There is no `+ 0.5` pixel-centre
 * correction: the WIT says the filter is ugly on purpose.
 *
 * The three stated edges, in the WIT and so in every guest: a destination over
 * 32 MiB is `malformed`; a zero-width or zero-height destination is a zero-pixel
 * image, not an error; and sampling a zero-area source into a destination with
 * area is `malformed`, because there is no pixel to copy -- and because
 * `dst_w / 0` is `Infinity` here rather than the trap it is elsewhere, which is
 * worse: an unchecked divide would fill the destination from `pixels[NaN]`,
 * which is `undefined`, which stores as 0. Silent black instead of a failure.
 *
 * # Why the loop may use plain numbers
 *
 * `dst_w * dst_h * 3` is checked in `BigInt` against the ceiling first, so past
 * that point `dst_w * dst_h <= 11184810`. `validate` has already established
 * `src_w * src_h * 3 === pixels.length`, so the source area is bounded by a
 * buffer the manifest's memory limit had to hold. Every product below is one of
 * those two areas times a factor under the other's dimension, which keeps them
 * many orders of magnitude under 2^53; and `Math.floor(N / D)` cannot cross an
 * integer boundary while `N < 2^52`, because the true quotient is then never
 * within an ulp of one. The exactness is argued, not assumed.
 */
function resize(image, extent) {
  const srcW = image.width;
  const srcH = image.height;
  const dstW = extent.width;
  const dstH = extent.height;

  if (dstW === 0 || dstH === 0) {
    return { width: dstW, height: dstH, pixels: new Uint8Array(0) };
  }
  if (srcW === 0 || srcH === 0) {
    throw malformed(
      `cannot resize ${srcW}x${srcH} to ${dstW}x${dstH}: no source pixels to sample`,
    );
  }

  // The one arithmetic difference between this guest and the other three, and
  // it is deliberate. Rust and C++ compute this total in `u64`, which wraps for
  // a destination whose exact byte count exceeds 2^64 -- reachable only from a
  // `u32` pair whose product is above 6.1e18, which no manifest's memory limit
  // could ever let past the loop below anyway. `BigInt` has no such edge, so
  // this refuses what the WIT says to refuse and prints the true number.
  const bytes = BigInt(dstW) * BigInt(dstH) * BigInt(CHANNELS);
  if (bytes > MAX_IMAGE_BYTES) {
    throw malformed(
      `resize to ${dstW}x${dstH} would need ${bytes} bytes, over the ${MAX_IMAGE_BYTES}-byte ceiling`,
    );
  }

  const src = image.pixels;
  const out = new Uint8Array(Number(bytes));
  let at = 0;
  for (let dy = 0; dy < dstH; dy += 1) {
    const sy = Math.floor((dy * srcH) / dstH);
    const row = sy * srcW * CHANNELS;
    for (let dx = 0; dx < dstW; dx += 1) {
      const from = row + Math.floor((dx * srcW) / dstW) * CHANNELS;
      out[at] = src[from];
      out[at + 1] = src[from + 1];
      out[at + 2] = src[from + 2];
      at += 3;
    }
  }

  return { width: dstW, height: dstH, pixels: out };
}

// ---------------------------------------------------------------------------
// The lookup table
// ---------------------------------------------------------------------------

/**
 * Rust's `str::lines`, reproduced: split on '\n', drop one trailing '\r', and do
 * not yield a final empty line for a string that ends in a newline.
 *
 * Not `String.prototype.split(/\r?\n/)` and emphatically not a regex with the
 * `s`-flag class: the entry numbering in every parse error depends on the guests
 * agreeing on what a line is, and JavaScript's line terminators include U+2028
 * and U+2029, which Rust's do not.
 */
function lines(text) {
  const parts = text.split('\n');
  if (parts.length > 0 && parts[parts.length - 1] === '') {
    parts.pop();
  }
  return parts.map((line) =>
    line.endsWith('\r') ? line.slice(0, -1) : line,
  );
}

/**
 * Rust's `str::split_ascii_whitespace`: space, tab, newline, form feed, carriage
 * return, and nothing else.
 *
 * Not `String.prototype.split(/\s+/)`, whose `\s` includes every Unicode space
 * plus the BOM -- one of those inside a line would make this guest read three
 * fields where the Rust guest read one and failed.
 */
function splitAsciiWhitespace(text) {
  const fields = [];
  let at = 0;
  const isSpace = (c) =>
    c === ' ' || c === '\t' || c === '\n' || c === '\f' || c === '\r';
  while (at < text.length) {
    while (at < text.length && isSpace(text[at])) at += 1;
    const start = at;
    while (at < text.length && !isSpace(text[at])) at += 1;
    if (at > start) fields.push(text.slice(start, at));
  }
  return fields;
}

/**
 * Rust's `u8::from_str`: an optional leading '+', then at least one ASCII digit,
 * and nothing else; the value must fit in 8 bits.
 *
 * Not `Number(field)` and not `parseInt`. `Number('')` is 0, `Number('0x10')` is
 * 16, `Number(' 3 ')` is 3, and `parseInt('12abc')` is 12 -- four ways to accept
 * a line the reference guest rejects. The range check *is* the parse, which is
 * why the error message can say "not three integers in 0..=255" for both a stray
 * character and a 256.
 */
function parseByte(field) {
  let at = 0;
  if (field[at] === '+') at += 1;
  if (at === field.length) return null;
  let value = 0;
  for (; at < field.length; at += 1) {
    const digit = field.charCodeAt(at) - 48;
    if (digit < 0 || digit > 9) return null;
    value = value * 10 + digit;
    if (value > 255) return null;
  }
  return value;
}

/** One `r g b` line as three bytes, or `null` if it is not one. */
function parseEntry(line) {
  const fields = splitAsciiWhitespace(line);
  if (fields.length !== 3) return null;
  const entry = fields.map(parseByte);
  return entry.some((byte) => byte === null) ? null : entry;
}

/**
 * Rust's `{:?}` for a `&str`, for the ASCII a lookup table can contain.
 *
 * The parse error quotes the offending line and the guests quote it the same
 * way -- not because the wording is normative (the WIT says it is not) but
 * because a reader diffing two guests' output should not have to wonder.
 * `JSON.stringify` is close and not close enough: it escapes with `\uXXXX`
 * where Rust writes `\u{x}`, and it escapes `\b` and `\f` differently.
 */
function escapeDebug(text) {
  let out = '"';
  for (const character of text) {
    const code = character.codePointAt(0);
    if (character === '\t') out += '\\t';
    else if (character === '\r') out += '\\r';
    else if (character === '\n') out += '\\n';
    else if (character === '\\') out += '\\\\';
    else if (character === '"') out += '\\"';
    else if (code < 0x20 || code === 0x7f) out += `\\u{${code.toString(16)}}`;
    else out += character;
  }
  return out + '"';
}

/**
 * Find the preopened directory a path lives under, and the path relative to it.
 *
 * **This function is what Rust's `std::fs` and wasi-libc's `fopen` do for the
 * other guests, and having to write it is the one real cost of this world in a
 * language with no libc.** WASI 0.2 has no `open` that takes an absolute path:
 * every open is `open-at` on a descriptor, and the only descriptors that exist
 * are the ones `get-directories` hands out. Rust and C++ never see this because
 * their runtimes do the prefix match inside `open`.
 *
 * The matching rule is the WIT's, under `operation.lut`: watoots preopens each
 * granted directory under the same string it was granted as, so the argument is
 * a path spelled the way the grant spells it and this only has to strip the
 * prefix. Longest prefix wins, in case two grants nest.
 *
 * `null` means no grant covers the path -- reported as not-found, which is what
 * WASI would have said anyway and for the better reason: a sandbox that
 * answered "exists, but you may not have it" would leak what it is hiding.
 */
function resolveInPreopen(path, directories) {
  let best = null;
  for (const [descriptor, prefix] of directories) {
    const base = prefix.endsWith('/') ? prefix : `${prefix}/`;
    if (!path.startsWith(base)) continue;
    if (best === null || base.length > best.base.length) {
      best = { descriptor, base, rest: path.slice(base.length) };
    }
  }
  return best;
}

const NOT_FOUND_HINT =
  '(a path outside every granted directory reports as not found, ' +
  'so check the manifest and the spelling of the path)';

/**
 * Read a file, refusing to read more than [`MAX_LUT_BYTES`] of it.
 *
 * # Handles are not garbage
 *
 * `get-directories` returns *owned* descriptors, one set per call, and `open-at`
 * returns another. In a language with destructors those go away at the end of a
 * scope; here they are entries in the component's resource table that outlive
 * every JavaScript object pointing at them unless something drops them. A `lut`
 * step called in a loop would leak one handle per preopen per call, and the
 * table would fill long before the manifest's memory limit noticed. So every
 * descriptor is disposed in a `finally`, which is this guest's version of the
 * frees `cpp-asset` writes out by hand.
 *
 * Errors come back as the reason string in `file-failure`. The WIT is explicit
 * that the prose is for people rather than hosts, so these say what this runtime
 * actually saw -- a WASI error-code name -- rather than impersonating another
 * guest's `strerror`.
 */
function readCapped(path) {
  const directories = getDirectories();
  let file = null;
  try {
    const found = resolveInPreopen(path, directories);
    if (found === null) {
      throw `cannot open it: no-entry ${NOT_FOUND_HINT}`;
    }

    try {
      file = found.descriptor.openAt(
        { symlinkFollow: true },
        found.rest,
        {},
        { read: true },
      );
    } catch (err) {
      const code = err?.payload ?? 'io';
      throw code === 'no-entry'
        ? `cannot open it: no-entry ${NOT_FOUND_HINT}`
        : `cannot open it: ${code}`;
    }

    const chunks = [];
    let total = 0;
    while (total < MAX_LUT_BYTES) {
      let chunk;
      let done;
      try {
        [chunk, done] = file.read(BigInt(MAX_LUT_BYTES - total), BigInt(total));
      } catch (err) {
        throw `cannot read it: ${err?.payload ?? 'io'}`;
      }
      if (chunk.length > 0) {
        chunks.push(chunk);
        total += chunk.length;
      }
      // `done` is end-of-file; a zero-length read that is not end-of-file would
      // otherwise spin forever on a file this plugin is not going to accept.
      if (done || chunk.length === 0) break;
    }

    const bytes = new Uint8Array(total);
    let at = 0;
    for (const chunk of chunks) {
      bytes.set(chunk, at);
      at += chunk.length;
    }

    try {
      // `fatal: true` on purpose. The Rust guest reads the table with
      // `read_to_string`, which refuses a file that is not UTF-8; a lenient
      // decode would replace the bad bytes and then fail somewhere less
      // informative, or -- worse -- parse.
      return new TextDecoder('utf-8', { fatal: true }).decode(bytes);
    } catch {
      throw 'cannot open it: stream did not contain valid UTF-8';
    }
  } finally {
    file?.[symbolDispose]?.();
    for (const [descriptor] of directories) {
      descriptor[symbolDispose]?.();
    }
  }
}

/**
 * Read and parse a LUT file.
 *
 * The format is the Rust guest's, documented on its `load_lut` and demonstrated
 * by `luts/sepia.lut`: 256 entries, one per line, each three decimal integers in
 * 0..=255 in red green blue order; blank lines and `#` comments ignored;
 * anything else must parse, and there must be exactly 256 entries -- a short
 * table would silently clip highlights rather than fail.
 *
 * Returns the table flattened into a `Uint8Array` of 768 bytes, because that is
 * what the lookup wants and an array of 256 three-element arrays is 256 more
 * objects for the collector to think about.
 */
function loadLut(path) {
  const text = readCapped(path);

  const table = new Uint8Array(256 * CHANNELS);
  let count = 0;
  const source = lines(text);
  for (let index = 0; index < source.length; index += 1) {
    // `String.prototype.trim` rather than an ASCII trim: it is the closest
    // thing here to Rust's Unicode `str::trim`, and differs from it only for a
    // leading byte-order mark, which no lookup table has.
    const line = source[index].trim();
    if (line === '' || line.startsWith('#')) continue;
    if (count === 256) {
      throw `more than 256 entries: a 257th appears on line ${index + 1}`;
    }
    const entry = parseEntry(line);
    if (entry === null) {
      throw `line ${index + 1} is not three integers in 0..=255: ${escapeDebug(line)}`;
    }
    table.set(entry, count * CHANNELS);
    count += 1;
  }

  if (count !== 256) {
    throw `expected 256 entries, found ${count}`;
  }
  return table;
}

/**
 * Apply a colour lookup table read from a file.
 *
 * This is the step with a capability cost. The plugin opens the file itself, so
 * the host has to grant `fs.read` over a directory the file is in, and the grant
 * is checked against the component's declared imports at *load* time -- before a
 * pixel is touched, and before this function has ever run.
 *
 * The argument is a path, spelled the way the grant spells it. That is the WIT's
 * rule, not this plugin's convention; `resolveInPreopen` is where this guest
 * pays for it.
 */
function lut(image, path) {
  let table;
  try {
    table = loadLut(path);
  } catch (reason) {
    // Only the strings thrown above are reasons. Anything else is a bug in this
    // plugin and should reach the host as a trap rather than be dressed up as
    // an unreadable file.
    if (typeof reason !== 'string') throw reason;
    throw unreadable(path, reason);
  }

  const pixels = image.pixels;
  for (let at = 0; at + 3 <= pixels.length; at += 3) {
    // Per channel, never across channels: red is looked up in the red column of
    // the entry the red input selects. Cross-channel mixing would make this a
    // matrix, and the file format is not one.
    pixels[at] = table[pixels[at] * CHANNELS];
    pixels[at + 1] = table[pixels[at + 1] * CHANNELS + 1];
    pixels[at + 2] = table[pixels[at + 2] * CHANNELS + 2];
  }
}

// ---------------------------------------------------------------------------
// The world's exports
// ---------------------------------------------------------------------------

function runStep(image, step) {
  switch (step.tag) {
    case 'grayscale':
      grayscale(image);
      return image;
    case 'invert':
      invert(image);
      return image;
    case 'gain':
      applyGain(image, step.val);
      return image;
    case 'resize':
      return resize(image, step.val);
    default:
      lut(image, step.val);
      return image;
  }
}

export function describe() {
  // `supports` is `list<operation-kind>`, so naming a step no longer means
  // constructing one. There is nothing here for a host to know to ignore, and
  // nothing for four guests to invent differently.
  return {
    name: 'js-asset',
    supports: ['grayscale', 'invert', 'gain', 'resize', 'lut'],
  };
}

export function apply(input, steps) {
  // Calling into the host: an import crossing, and one of the things the
  // recorder captures. Deterministic on purpose -- no counts that depend on
  // anything but the arguments.
  emit('info', `apply: ${input.width}x${input.height}, ${steps.length} step(s)`);

  // `input.pixels` is a fresh `Uint8Array` the bindings copied out of linear
  // memory for this call, so the operations below mutate it in place rather than
  // allocating a second image per step. That is the same move the Rust guest
  // gets from taking `Image` by value and the C++ one writes out by hand.
  let image = input;

  try {
    validate(image);

    // "Steps apply in order. The whole call fails on the first step that cannot
    // be done" -- so a plain loop that throws, and a partly transformed image
    // never escapes.
    for (const step of steps) {
      image = runStep(image, step);
    }
  } catch (refusal) {
    if (refusal instanceof Refusal) throw report(refusal);
    throw refusal;
  }

  return image;
}
