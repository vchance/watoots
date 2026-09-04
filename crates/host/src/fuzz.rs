//! Type-driven generation of component-model values.
//!
//! A plugin host has no opinion about what a plugin *should* return, so "wrong
//! answer" is not a signal a fuzzer can use. What it can use is the component's
//! own types: take the parameter types off `Func::ty()`, build a value for each
//! one, and every generated call is type-correct by construction. Fuzzing a
//! component with random *bytes* would spend its whole budget being rejected by
//! the canonical ABI, which is Wasmtime's code and already fuzzed upstream.
//! See `docs/adr/0008-fuzzing.md`.
//!
//! Generation is driven by a finite buffer of bytes rather than by a random
//! number generator, for two reasons. It keeps this module free of
//! dependencies — a published library should not carry a test framework into
//! everybody's build — and it gives both callers the shape they want: the
//! property tests hand it bytes proptest can shrink, and `watoots fuzz` hands
//! it bytes expanded from a `--seed`. Neither reimplements the other.
//!
//! ```no_run
//! use watoots::fuzz::Generator;
//! # fn main() -> Result<(), watoots::Error> {
//! # let mut plugin: watoots::Plugin = unimplemented!();
//! let params = plugin.export_params("lint")?;
//! let mut generator = Generator::from_seed(7);
//! let args = generator.values(&params)?;
//! let _ = plugin.call("lint", &args);
//! # Ok(())
//! # }
//! ```
//!
//! # What it refuses
//!
//! Resource handles, futures, streams, error-contexts and maps have no WAVE
//! spelling, so a value of one of those types cannot be written into a trace —
//! see `docs/adr/0004-wave-and-dynamic-typing.md`. The generator refuses them
//! by name rather than producing something that fails three layers down for a
//! reason that does not mention resources.

use wasmtime::component::{Type, Val};

use crate::{Error, ErrorKind, Result};

/// How deeply a generated value may nest before the generator stops recursing.
///
/// At the floor, lists come back empty and options come back `none`, so
/// generation always terminates even on a self-referential-looking world.
pub const DEFAULT_MAX_DEPTH: u32 = 4;

/// How many elements a generated list, string or fixed-length list may hold.
pub const DEFAULT_MAX_ELEMENTS: usize = 8;

/// How many bytes [`Generator::from_seed`] expands a seed into.
pub const DEFAULT_ENTROPY_BYTES: usize = 1024;

/// Interesting strings, which is where a text-based trace format breaks if it
/// is going to. Quotes, backslashes and newlines have to survive WAVE's escapes
/// and then the trace's own one-value-per-line rule; the keyword-shaped ones
/// are there because the text encoding parses lines by their first word.
const INTERESTING_STRINGS: &[&str] = &[
    "",
    " ",
    "  ",
    "\n",
    "\r\n",
    "\t",
    "\"",
    "\\",
    "\"\\\n\t\r",
    "arg ",
    "value ",
    "unit",
    "  arg \"x\"",
    "error WT_ERR_TRAP \"boom\"",
    "watoots:example/log@0.1.0 emit",
    "watoots-trace 1",
    "\0",
    "\u{1}\u{7f}",
    "héllo",
    "日本語",
    "🦀",
    "\u{feff}",
    "a\u{85}b",
    "a\u{2028}b",
    "unresolved TODO",
];

/// Interesting chars: the escape-worthy ones, both ends of the surrogate gap,
/// and the top of the range.
#[rustfmt::skip]
const INTERESTING_CHARS: &[char] = &[
    '\0', '\n', '\r', '\t', '"', '\\', '\'', ' ', 'a', '~',
    '\u{7f}', '\u{80}', 'é', '日', '🦀',
    '\u{d7ff}', '\u{e000}', '\u{10ffff}', '\u{feff}', '\u{2028}', '\u{85}',
];

#[rustfmt::skip]
const INTERESTING_F32: &[f32] = &[
    0.0, -0.0, 1.0, -1.0, 0.1, -0.1,
    f32::INFINITY, f32::NEG_INFINITY, f32::NAN,
    f32::MIN, f32::MAX, f32::EPSILON, f32::MIN_POSITIVE,
];

#[rustfmt::skip]
const INTERESTING_F64: &[f64] = &[
    0.0, -0.0, 1.0, -1.0, 0.1, -0.1,
    f64::INFINITY, f64::NEG_INFINITY, f64::NAN,
    f64::MIN, f64::MAX, f64::EPSILON, f64::MIN_POSITIVE,
];

/// Interesting unsigned magnitudes; the signed types reinterpret these, so one
/// table covers both edges of every width.
#[rustfmt::skip]
const INTERESTING_INTS: &[u64] = &[
    0, 1, 2, 7,
    0x7f, 0x80, 0xff,
    0x7fff, 0x8000, 0xffff,
    0x7fff_ffff, 0x8000_0000, 0xffff_ffff,
    0x7fff_ffff_ffff_ffff, 0x8000_0000_0000_0000, 0xffff_ffff_ffff_ffff,
];

/// Builds component-model values from a component's own types.
///
/// The buffer is finite and never refilled: once it runs out every draw reads
/// zero, which makes lists empty, options `none` and numbers `0`. A generator
/// therefore always terminates, and a shrunk (shorter) buffer yields a simpler
/// value, which is what makes proptest's shrinking mean something here.
#[derive(Debug, Clone)]
pub struct Generator {
    bytes: Vec<u8>,
    at: usize,
    max_depth: u32,
    max_elements: usize,
}

impl Generator {
    /// Draw from these bytes.
    #[must_use]
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self {
            bytes,
            at: 0,
            max_depth: DEFAULT_MAX_DEPTH,
            max_elements: DEFAULT_MAX_ELEMENTS,
        }
    }

    /// Draw from [`DEFAULT_ENTROPY_BYTES`] bytes expanded from a seed.
    ///
    /// The expansion is SplitMix64, written out here rather than pulled in:
    /// this has to be identical from build to build so that a `--seed` in a bug
    /// report reproduces, and a dependency that reserves the right to change
    /// its algorithm cannot promise that.
    #[must_use]
    pub fn from_seed(seed: u64) -> Self {
        Self::from_bytes(seed_bytes(seed, DEFAULT_ENTROPY_BYTES))
    }

    /// Cap how deeply generated values nest. Zero means no nesting at all.
    #[must_use]
    pub fn max_depth(mut self, depth: u32) -> Self {
        self.max_depth = depth;
        self
    }

    /// Cap the length of generated lists, strings and fixed-length lists.
    #[must_use]
    pub fn max_elements(mut self, elements: usize) -> Self {
        self.max_elements = elements;
        self
    }

    /// Whether the buffer is spent. Everything drawn from here on is zero.
    #[must_use]
    pub fn is_exhausted(&self) -> bool {
        self.at >= self.bytes.len()
    }

    /// Build one value of this type.
    ///
    /// Fails, naming the type, for the ones WAVE cannot spell.
    pub fn value(&mut self, ty: &Type) -> Result<Val> {
        let depth = self.max_depth;
        self.value_at(ty, depth)
    }

    /// Build one value for each type, in order — a whole argument list.
    pub fn values(&mut self, types: &[Type]) -> Result<Vec<Val>> {
        types.iter().map(|ty| self.value(ty)).collect()
    }

    fn value_at(&mut self, ty: &Type, depth: u32) -> Result<Val> {
        Ok(match ty {
            Type::Bool => Val::Bool(self.byte() & 1 == 1),
            Type::S8 => Val::S8(self.int() as i8),
            Type::U8 => Val::U8(self.int() as u8),
            Type::S16 => Val::S16(self.int() as i16),
            Type::U16 => Val::U16(self.int() as u16),
            Type::S32 => Val::S32(self.int() as i32),
            Type::U32 => Val::U32(self.int() as u32),
            Type::S64 => Val::S64(self.int() as i64),
            Type::U64 => Val::U64(self.int()),
            Type::Float32 => Val::Float32(self.float32()),
            Type::Float64 => Val::Float64(self.float64()),
            Type::Char => Val::Char(self.character()),
            Type::String => Val::String(self.text()),

            Type::List(list) => {
                let element = list.ty();
                let count = self.count(depth);
                Val::List(self.repeat(&element, count, depth)?)
            }
            Type::FixedLengthList(list) => {
                // The length is the type's, not ours: a fixed-length list of
                // the wrong length is not a value of that type at all.
                let element = list.ty();
                let count = list.len() as usize;
                Val::FixedLengthList(self.repeat(&element, count, depth)?)
            }
            Type::Tuple(tuple) => {
                let types: Vec<Type> = tuple.types().collect();
                let mut values = Vec::with_capacity(types.len());
                for element in &types {
                    values.push(self.value_at(element, depth.saturating_sub(1))?);
                }
                Val::Tuple(values)
            }
            Type::Record(record) => {
                let fields: Vec<(String, Type)> = record
                    .fields()
                    .map(|field| (field.name.to_string(), field.ty))
                    .collect();
                let mut values = Vec::with_capacity(fields.len());
                for (name, field_ty) in &fields {
                    values.push((
                        name.clone(),
                        self.value_at(field_ty, depth.saturating_sub(1))?,
                    ));
                }
                Val::Record(values)
            }
            Type::Variant(variant) => {
                let cases: Vec<(String, Option<Type>)> = variant
                    .cases()
                    .map(|case| (case.name.to_string(), case.ty))
                    .collect();
                if cases.is_empty() {
                    return Err(unrepresentable("a variant with no cases", ty));
                }
                let (name, payload) = &cases[self.pick(cases.len())];
                let value = match payload {
                    None => None,
                    Some(payload) => {
                        Some(Box::new(self.value_at(payload, depth.saturating_sub(1))?))
                    }
                };
                Val::Variant(name.clone(), value)
            }
            Type::Enum(enum_) => {
                let names: Vec<String> = enum_.names().map(str::to_string).collect();
                if names.is_empty() {
                    return Err(unrepresentable("an enum with no cases", ty));
                }
                Val::Enum(names[self.pick(names.len())].clone())
            }
            Type::Option(option) => {
                let inner = option.ty();
                if depth == 0 || self.byte() & 1 == 0 {
                    Val::Option(None)
                } else {
                    Val::Option(Some(Box::new(
                        self.value_at(&inner, depth.saturating_sub(1))?,
                    )))
                }
            }
            Type::Result(result) => {
                let (ok, err) = (result.ok(), result.err());
                let take_ok = self.byte() & 1 == 0;
                let payload = if take_ok { ok.as_ref() } else { err.as_ref() };
                let value = match payload {
                    None => None,
                    Some(payload) => {
                        Some(Box::new(self.value_at(payload, depth.saturating_sub(1))?))
                    }
                };
                Val::Result(if take_ok { Ok(value) } else { Err(value) })
            }
            Type::Flags(flags) => {
                // Kept in the type's declared order. A flag set is a set, but
                // its rendering is a list, and reordering it would make a
                // round-trip failure look like a bug in WAVE rather than one
                // here.
                let names: Vec<String> = flags.names().map(str::to_string).collect();
                let mut set = Vec::new();
                for name in names {
                    if self.byte() & 1 == 1 {
                        set.push(name);
                    }
                }
                Val::Flags(set)
            }

            Type::Own(_) | Type::Borrow(_) => {
                return Err(unrepresentable("a resource handle", ty));
            }
            Type::Future(_) => return Err(unrepresentable("a future", ty)),
            Type::Stream(_) => return Err(unrepresentable("a stream", ty)),
            Type::ErrorContext => return Err(unrepresentable("an error-context", ty)),
            Type::Map(_) => return Err(unrepresentable("a map", ty)),
        })
    }

    fn repeat(&mut self, element: &Type, count: usize, depth: u32) -> Result<Vec<Val>> {
        let mut values = Vec::with_capacity(count.min(self.max_elements.max(1)));
        for _ in 0..count {
            values.push(self.value_at(element, depth.saturating_sub(1))?);
        }
        Ok(values)
    }

    /// How many elements to put in a list, given the depth left.
    fn count(&mut self, depth: u32) -> usize {
        if depth == 0 || self.max_elements == 0 {
            return 0;
        }
        self.pick(self.max_elements + 1)
    }

    /// The next byte, or zero once the buffer is spent.
    fn byte(&mut self) -> u8 {
        let byte = self.bytes.get(self.at).copied().unwrap_or(0);
        self.at = self.at.saturating_add(1);
        byte
    }

    /// A number in `0..bound`, biased toward small values by construction.
    fn pick(&mut self, bound: usize) -> usize {
        if bound <= 1 {
            return 0;
        }
        usize::from(self.byte()) % bound
    }

    fn raw(&mut self, width: usize) -> u64 {
        let mut value = 0u64;
        for index in 0..width {
            value |= u64::from(self.byte()) << (8 * index);
        }
        value
    }

    /// An integer: half the time from the interesting table, half raw bits.
    fn int(&mut self) -> u64 {
        if self.byte() & 1 == 0 {
            INTERESTING_INTS[self.pick(INTERESTING_INTS.len())]
        } else {
            self.raw(8)
        }
    }

    fn float32(&mut self) -> f32 {
        if self.byte() & 1 == 0 {
            INTERESTING_F32[self.pick(INTERESTING_F32.len())]
        } else {
            f32::from_bits(self.raw(4) as u32)
        }
    }

    fn float64(&mut self) -> f64 {
        if self.byte() & 1 == 0 {
            INTERESTING_F64[self.pick(INTERESTING_F64.len())]
        } else {
            f64::from_bits(self.raw(8))
        }
    }

    fn character(&mut self) -> char {
        if self.byte() & 1 == 0 {
            INTERESTING_CHARS[self.pick(INTERESTING_CHARS.len())]
        } else {
            let raw = self.raw(4) as u32;
            // Surrogates are not chars. Folding rather than retrying keeps the
            // number of bytes a value costs independent of what they are, which
            // is what lets a shrunk buffer shrink the value.
            char::from_u32(raw)
                .unwrap_or_else(|| char::from_u32(raw % 0xd800).expect("below the surrogate range"))
        }
    }

    fn text(&mut self) -> String {
        if self.byte() & 1 == 0 {
            return INTERESTING_STRINGS[self.pick(INTERESTING_STRINGS.len())].to_string();
        }
        let count = self.pick(self.max_elements + 1);
        (0..count).map(|_| self.character()).collect()
    }
}

/// Whether every type in a list is one this generator can build.
///
/// `watoots fuzz` calls this before doing any work, so a world with resources
/// in it is refused with the reason up front rather than after a compile.
pub fn supported(types: &[Type]) -> Result<()> {
    for ty in types {
        supported_one(ty, DEFAULT_MAX_DEPTH)?;
    }
    Ok(())
}

fn supported_one(ty: &Type, depth: u32) -> Result<()> {
    if depth == 0 {
        return Ok(());
    }
    let next = depth - 1;
    match ty {
        Type::Bool
        | Type::S8
        | Type::U8
        | Type::S16
        | Type::U16
        | Type::S32
        | Type::U32
        | Type::S64
        | Type::U64
        | Type::Float32
        | Type::Float64
        | Type::Char
        | Type::String
        | Type::Enum(_)
        | Type::Flags(_) => Ok(()),
        Type::List(list) => supported_one(&list.ty(), next),
        Type::FixedLengthList(list) => supported_one(&list.ty(), next),
        Type::Option(option) => supported_one(&option.ty(), next),
        Type::Tuple(tuple) => tuple.types().try_for_each(|ty| supported_one(&ty, next)),
        Type::Record(record) => record
            .fields()
            .try_for_each(|field| supported_one(&field.ty, next)),
        Type::Variant(variant) => variant
            .cases()
            .filter_map(|case| case.ty)
            .try_for_each(|ty| supported_one(&ty, next)),
        Type::Result(result) => {
            for payload in [result.ok(), result.err()].into_iter().flatten() {
                supported_one(&payload, next)?;
            }
            Ok(())
        }
        Type::Own(_) | Type::Borrow(_) => Err(unrepresentable("a resource handle", ty)),
        Type::Future(_) => Err(unrepresentable("a future", ty)),
        Type::Stream(_) => Err(unrepresentable("a stream", ty)),
        Type::ErrorContext => Err(unrepresentable("an error-context", ty)),
        Type::Map(_) => Err(unrepresentable("a map", ty)),
    }
}

/// Expand a seed into bytes with SplitMix64.
///
/// Public because a caller reproducing a reported crash needs the same bytes
/// this build produced, and because `watoots fuzz` derives one buffer per case
/// from `seed + case`.
#[must_use]
pub fn seed_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed;
    let mut out = Vec::with_capacity(len);
    while out.len() < len {
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^= z >> 31;
        out.extend_from_slice(&z.to_le_bytes());
    }
    out.truncate(len);
    out
}

/// The one refusal this module makes, spelled the same way every time.
fn unrepresentable(what: &str, ty: &Type) -> Error {
    Error::new(
        ErrorKind::InvalidArgument,
        format!(
            "cannot generate {what}: it has no WAVE spelling, so a generated \
             value could not be recorded in a trace. Worlds using resources, \
             futures, streams, error-contexts or maps cannot be fuzzed for the \
             same reason they cannot be traced — see \
             docs/adr/0004-wave-and-dynamic-typing.md. Offending type: {ty:?}"
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_seed_expands_the_same_way_every_time() {
        // A `--seed` in a bug report is worthless if this drifts.
        assert_eq!(seed_bytes(7, 32), seed_bytes(7, 32));
        assert_ne!(seed_bytes(7, 32), seed_bytes(8, 32));
        assert_eq!(seed_bytes(7, 32).len(), 32);
        assert_eq!(seed_bytes(7, 5).len(), 5);
    }

    #[test]
    fn an_exhausted_generator_keeps_answering() {
        // Termination does not depend on how much entropy a caller supplied.
        let mut generator = Generator::from_bytes(Vec::new());
        assert!(generator.is_exhausted());
        assert_eq!(generator.byte(), 0);
        assert_eq!(generator.pick(4), 0);
        assert_eq!(generator.text(), "");
    }

    #[test]
    fn raw_bits_that_are_not_characters_are_folded_rather_than_skipped() {
        // A surrogate is not a `char`, so the fold has to be exercised: if it
        // retried instead, the number of bytes a char costs would depend on
        // what those bytes were, and a shrunk buffer would stop producing a
        // related value.
        let mut generator = Generator::from_bytes(vec![
            0x01, 0x00, 0xd9, 0x00, 0x00, // odd selector, then 0xd900 raw
        ]);
        assert_eq!(generator.character(), '\u{100}');
    }

    #[test]
    fn generation_spends_a_bounded_number_of_bytes() {
        // The buffer is what bounds the work, so nothing may loop on it.
        let mut generator = Generator::from_bytes(seed_bytes(3, 256)).max_elements(4);
        for _ in 0..64 {
            let _ = generator.text();
            let _ = generator.character();
            let _ = generator.int();
        }
        assert!(generator.is_exhausted());
    }
}
