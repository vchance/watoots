"""A sample watoots plugin: an image-operation pipeline, in Python.

    tools/build-plugins.sh py-asset

The same world as `examples/plugins/rust-asset`, and the same *bytes*.
`examples/wit/asset/asset.wit` says four languages have to agree on every
output byte; this is the fourth of them, and the only one whose runtime is an
entire other language implementation.

Where `py-lint` shows a Python guest that needs no capability, this one needs
exactly one: `lut` opens a file itself, so `fs.read` has to be granted and
scoped, and a manifest that does not grant it fails the *load*. Unlike the
JavaScript guest, this one needs no help from its world to do it -- CPython is
linked against wasi-libc, which does the preopen prefix match inside `open`.

# Everything here is a transcription, not a decision

`examples/plugins/rust-asset/src/lib.rs` is the reference implementation and
documents every arithmetic rule on the function that implements it. This file
implements the same rules and says so at each site; where Python offers a
shorter spelling that would round differently, the docstring says which one was
rejected and why. The rules, once more:

    grayscale  y = (299*r + 587*g + 114*b + 500) // 1000, integer division
    invert     255 - c
    gain       min(255, floor(c * factor + 0.5)) in f64
    resize     sx = dx * src_w // dst_w, integer division; likewise sy
    lut        out.c = table[in.c].c, per channel, never across channels

Three of those are traps in this language specifically:

  * `round` is half-to-even -- `round(0.5)` is `0` and `round(1.5)` is `2` --
    and **this is the guest the world's rounding rule was written for**. Rust's
    `f64::round` (half-away-from-zero) and JavaScript's `Math.round`
    (half-toward-+inf) both give the same answer as `floor(x + 0.5)` on the
    non-negative operands `gain` produces, so those two would have agreed by
    coincidence. Python's would not. `round` appears nowhere below.
  * `/` is float division even between two integers. `grayscale` and `resize`
    both use `//`, and a `/` in either would be a silently different filter.
  * `int` is arbitrary precision, which makes every overflow question in the
    other three guests simply not arise. That is the one place this language
    makes the contract *easier*, and it is worth saying out loud because the
    same freedom is why `//` has to be written deliberately: the language will
    not stop you widening into a float.

Nothing here reads a clock or asks for randomness. The same input produces the
same bytes on every run.
"""

import math
from typing import List, Optional, Tuple

import wit_world
from componentize_py_types import Err
from wit_world.imports import log, types
from wit_world.imports.log import Level
from wit_world.imports.manifest import PluginInfo
from wit_world.imports.types import (
    Channel,
    Failure,
    Failure_Malformed,
    Failure_Unreadable,
    FileFailure,
    Gain,
    Image,
    Operation,
    Operation_Gain,
    Operation_Grayscale,
    Operation_Invert,
    Operation_Lut,
    Operation_Resize,
    OperationKind,
)

# Bytes per pixel. RGB8, no alpha -- the WIT says so.
CHANNELS = 3

# Ceiling on a `resize` destination, in bytes. **32 MiB, from the WIT.**
#
# Transcribed from `operation.resize` in `asset.wit`, the same way the other
# guests transcribe it. Changing it here would be a silent disagreement with the
# world rather than a tuning decision.
MAX_IMAGE_BYTES = 32 * 1024 * 1024

# Ceiling on the size of a lookup-table file this plugin will read.
#
# Matches `MAX_LUT_BYTES` in the Rust guest, and for the same reason: a grant is
# a directory, so "what is in luts/" is not a closed set, and reading all of
# whatever someone drops there would exhaust the manifest's memory limit. A file
# that hits the cap is truncated and then fails the 256-entry check.
MAX_LUT_BYTES = 64 * 1024

# Rust's `char::is_ascii_whitespace`: space, tab, newline, form feed, carriage
# return. Not `str.isspace()`, which is Unicode and would let this guest read
# three fields on a line where the reference guest read one and failed.
ASCII_SPACE = " \t\n\x0c\r"


class Refusal(Exception):
    """A failure on its way out of `apply`.

    componentize-py turns a raised `componentize_py_types.Err` into the `err`
    arm and anything else into a trap. This carries the `failure` as far as
    `apply`, which logs it once and re-raises it as an `Err` -- the same
    division `report` makes in the Rust guest, and the reason a bug in this file
    still reaches the host as a trap rather than being dressed up as an answer.
    """

    def __init__(self, failure: Failure) -> None:
        super().__init__()
        self.failure = failure


def malformed(reason: str) -> Refusal:
    return Refusal(Failure_Malformed(reason))


def unreadable(path: str, reason: str) -> Refusal:
    return Refusal(Failure_Unreadable(FileFailure(path=path, reason=reason)))


# ---------------------------------------------------------------------------
# The operations
#
# Each takes and returns a `bytearray`, because `image.pixels` is `bytes` and
# `bytes` is immutable. That is this language's version of the ownership note in
# `cpp-asset`: one mutable buffer per call, converted back at the boundary,
# rather than a fresh copy per step.
# ---------------------------------------------------------------------------


def validate(width: int, height: int, pixels: bytes) -> None:
    """Check that an image is an image before touching a byte of it.

    A pixel buffer whose length disagrees with the dimensions is exactly the
    input that turns an indexing bug into an exception, and an uncaught
    exception in a guest is a trap. Untrusted input arrives here too.

    Exact without effort: `width * height * 3` can reach 5.5e19 and Python's
    `int` does not care. The other three guests each had to pick a width for it.
    """
    expected = width * height * CHANNELS
    actual = len(pixels)
    if actual != expected:
        raise malformed(f"{width}x{height} is {expected} bytes of RGB8, got {actual}")


def grayscale(pixels: bytearray) -> bytearray:
    """Rec. 601 luma, replicated across all three channels.

        y = (299*r + 587*g + 114*b + 500) // 1000

    with truncating integer division. Matching the Rust guest, which explains
    the choice at length: the coefficients are the Rec. 601 weights scaled by
    1000, the `+ 500` is half the divisor so the division rounds half up, and
    every term is non-negative so "half up" and "half away from zero" are the
    same rule.

    `//` and not `/`. `/` between two `int`s is float division in Python 3, so
    the obvious transcription of this line would produce a float, and
    `bytearray` assignment would reject it -- loudly here, which is lucky.
    `//` on non-negative operands is the truncating division the world means;
    it floors, and the two agree because nothing here is negative.

    Deliberately not `0.299 * r + ...`. That is the spelling Python makes
    easiest and it is the one that disagrees with the other three guests.
    """
    for at in range(0, len(pixels) - 2, CHANNELS):
        luma = (
            299 * pixels[at] + 587 * pixels[at + 1] + 114 * pixels[at + 2] + 500
        ) // 1000
        pixels[at] = luma
        pixels[at + 1] = luma
        pixels[at + 2] = luma
    return pixels


def invert(pixels: bytearray) -> bytearray:
    """`255 - c`, per channel. No rounding: the operation is exact in 8 bits."""
    for at in range(len(pixels)):
        pixels[at] = 255 - pixels[at]
    return pixels


def apply_gain(pixels: bytearray, gain: Gain) -> bytearray:
    """Multiply one channel by a factor and clamp to 0..=255.

        out = min(255, floor(c * factor + 0.5))

    in f64, matching the Rust guest exactly. Three things make that
    reproducible:

     1. The factor is an `f32` in the WIT. componentize-py lifts it into a
        Python `float`, which is an f64 holding exactly the widened `f32` -- the
        same widening every other guest performs before it multiplies. The
        multiply is then one IEEE-754 operation, correctly rounded, identical
        everywhere.
     2. `math.floor(x + 0.5)`, **not** `round`. This is the one guest where
        that substitution actually changes an answer: `round` is half-to-even,
        so `round(2.5)` is `2` where every other guest's default gives `3`.
        Rust's `f64::round` and JavaScript's `Math.round` would both have
        matched `floor(x + 0.5)` here by accident, which is precisely why the
        world states the rule instead of trusting the accident. (`int(x + 0.5)`
        would truncate rather than floor, which is the same thing only while
        nothing is negative -- true here, and not a reason to write the weaker
        line.)
     3. The factor is clamped to 0.0..=4.0 and NaN is treated as 0.0. That is
        the WIT's rule, stated on `gain.factor`. The NaN test comes first
        because `min`/`max` on a NaN in Python return whichever operand they
        happened to see first, so a clamp alone would let it through -- and
        `math.floor(nan)` raises `ValueError`, which would reach the host as a
        trap rather than as an answer.

    `min(255, ...)` before the store: the product can reach 255 * 4 = 1020, and
    `bytearray` assignment raises `ValueError` outside 0..=255. Another trap
    where Rust merely saturates.
    """
    factor = gain.factor
    if math.isnan(factor):
        factor = 0.0
    else:
        factor = min(4.0, max(0.0, factor))

    offset = {Channel.RED: 0, Channel.GREEN: 1, Channel.BLUE: 2}[gain.channel]

    for at in range(offset, len(pixels) - 2 + offset, CHANNELS):
        pixels[at] = min(255, math.floor(pixels[at] * factor + 0.5))
    return pixels


def resize(
    width: int, height: int, pixels: bytearray, extent: types.Extent
) -> Tuple[int, int, bytearray]:
    """Nearest neighbour, top-left biased.

    Every rule is `operation.resize` in `asset.wit`; this implements the world
    rather than deciding it.

        sx = dx * src_w // dst_w
        sy = dy * src_h // dst_h

    with truncating integer division. There is no `+ 0.5` pixel-centre
    correction: the WIT says the filter is ugly on purpose.

    The three stated edges, in the WIT and so in every guest: a destination over
    32 MiB is `malformed`; a zero-width or zero-height destination is a
    zero-pixel image, not an error; and sampling a zero-area source into a
    destination with area is `malformed`, because there is no pixel to copy --
    and because `dst_w // 0` is a `ZeroDivisionError`, which would reach the
    host as a trap.

    Nothing here can overflow. `int` is arbitrary precision, so the byte total
    and every index are exact for any `u32` pair the host can send. Rust and C++
    compute the same total in `u64`, which wraps above 2^64 -- reachable only
    from a `u32` pair whose product exceeds 6.1e18, and refused long before the
    loop by any manifest that exists.
    """
    if extent.width == 0 or extent.height == 0:
        return extent.width, extent.height, bytearray()
    if width == 0 or height == 0:
        raise malformed(
            f"cannot resize {width}x{height} to {extent.width}x{extent.height}: "
            "no source pixels to sample"
        )

    total = extent.width * extent.height * CHANNELS
    if total > MAX_IMAGE_BYTES:
        raise malformed(
            f"resize to {extent.width}x{extent.height} would need {total} bytes, "
            f"over the {MAX_IMAGE_BYTES}-byte ceiling"
        )

    out = bytearray(total)
    at = 0
    for dy in range(extent.height):
        row = (dy * height // extent.height) * width * CHANNELS
        for dx in range(extent.width):
            source = row + (dx * width // extent.width) * CHANNELS
            out[at : at + CHANNELS] = pixels[source : source + CHANNELS]
            at += CHANNELS

    return extent.width, extent.height, out


# ---------------------------------------------------------------------------
# The lookup table
# ---------------------------------------------------------------------------


def lines(text: str) -> List[str]:
    """Rust's `str.lines`, reproduced.

    Split on '\\n', drop one trailing '\\r', and do not yield a final empty line
    for a string that ends in a newline.

    Emphatically not `str.splitlines()`, which also splits on '\\v', '\\f',
    '\\x1c'..'\\x1e', U+0085, U+2028 and U+2029. The entry numbering in every
    parse error depends on the guests agreeing on what a line is, and a form
    feed inside a comment would be enough to make them disagree.
    """
    parts = text.split("\n")
    if parts and parts[-1] == "":
        parts.pop()
    return [line[:-1] if line.endswith("\r") else line for line in parts]


def split_ascii_whitespace(text: str) -> List[str]:
    """Rust's `str::split_ascii_whitespace`.

    Not bare `str.split()`, whose separator set is Unicode: a non-breaking space
    inside a line would give this guest three fields where the reference guest
    saw one and refused the table.
    """
    fields: List[str] = []
    at = 0
    while at < len(text):
        while at < len(text) and text[at] in ASCII_SPACE:
            at += 1
        start = at
        while at < len(text) and text[at] not in ASCII_SPACE:
            at += 1
        if at > start:
            fields.append(text[start:at])
    return fields


def parse_byte(field: str) -> Optional[int]:
    """Rust's `u8::from_str`.

    An optional leading '+', then at least one ASCII digit, and nothing else;
    the value must fit in 8 bits.

    Not `int(field)`, which accepts '_' separators, surrounding whitespace, a
    leading '-', and every Unicode decimal digit there is -- `int("١٢")` is 12.
    Four ways to accept a line the reference guest rejects. The range check *is*
    the parse, which is why the error message can say "not three integers in
    0..=255" for both a stray character and a 256.
    """
    at = 1 if field.startswith("+") else 0
    if at == len(field):
        return None
    value = 0
    for character in field[at:]:
        if character < "0" or character > "9":
            return None
        value = value * 10 + (ord(character) - ord("0"))
        if value > 255:
            return None
    return value


def parse_entry(line: str) -> Optional[List[int]]:
    """One `r g b` line as three bytes, or `None` if it is not one."""
    fields = split_ascii_whitespace(line)
    if len(fields) != CHANNELS:
        return None
    entry = [parse_byte(field) for field in fields]
    if any(byte is None for byte in entry):
        return None
    return [byte for byte in entry if byte is not None]


def escape_debug(text: str) -> str:
    """Rust's `{:?}` for a `&str`, for the ASCII a lookup table can contain.

    The parse error quotes the offending line and the guests quote it the same
    way -- not because the wording is normative (the WIT says it is not) but
    because a reader diffing two guests' output should not have to wonder.
    `repr()` is close and not close enough: it prefers single quotes, escapes
    with `\\xNN` where Rust writes `\\u{n}`, and leaves a double quote unescaped.
    """
    out = ['"']
    for character in text:
        if character == "\t":
            out.append("\\t")
        elif character == "\r":
            out.append("\\r")
        elif character == "\n":
            out.append("\\n")
        elif character == "\\":
            out.append("\\\\")
        elif character == '"':
            out.append('\\"')
        elif ord(character) < 0x20 or ord(character) == 0x7F:
            out.append(f"\\u{{{ord(character):x}}}")
        else:
            out.append(character)
    out.append('"')
    return "".join(out)


def read_capped(path: str) -> str:
    """Read a file, refusing to read more than `MAX_LUT_BYTES` of it.

    `open` on the string as given and nothing more. The argument is a path
    spelled the way the grant spells it -- the WIT's rule, under
    `operation.lut` -- and wasi-libc does the preopen prefix match inside
    `open`, exactly as it does for the C++ guest and as Rust's `std::fs` does
    for the reference one. This is the whole of what the JavaScript guest has to
    write by hand.

    The reason strings are this runtime's own words. The WIT is explicit that
    the prose in `file-failure` is for people rather than hosts, so there is
    nothing to be gained by impersonating another guest's `io::Error`
    formatting -- but "the manifest is usually the answer" is said out loud,
    because a path outside every preopened directory arrives here as
    `FileNotFoundError`, byte-identical to a missing file inside a granted one.
    A sandbox that answered "exists, but you may not have it" would leak what it
    is hiding, so the guest cannot tell the two apart and does not pretend to.
    """
    try:
        with open(path, "rb") as handle:
            raw = handle.read(MAX_LUT_BYTES)
    except FileNotFoundError as err:
        raise unreadable(
            path,
            f"cannot open it: {err.strerror} (a path outside every granted "
            "directory reports as not found, so check the manifest and the "
            "spelling of the path)",
        ) from None
    except OSError as err:
        raise unreadable(path, f"cannot open it: {err.strerror}") from None

    try:
        # `decode` is strict by default, which is what is wanted: the Rust guest
        # reads the table with `read_to_string`, which refuses a file that is
        # not UTF-8. A lenient decode would replace the bad bytes and then fail
        # somewhere less informative, or -- worse -- parse.
        return raw.decode("utf-8")
    except UnicodeDecodeError:
        raise unreadable(
            path, "cannot open it: stream did not contain valid UTF-8"
        ) from None


def load_lut(path: str) -> bytes:
    """Read and parse a LUT file.

    The format is the Rust guest's, documented on its `load_lut` and
    demonstrated by `luts/sepia.lut`: 256 entries, one per line, each three
    decimal integers in 0..=255 in red green blue order; blank lines and `#`
    comments ignored; anything else must parse, and there must be exactly 256
    entries -- a short table would silently clip highlights rather than fail.

    Returned flattened to 768 bytes, because that is what the lookup wants.
    """
    table = bytearray()
    count = 0
    for index, raw in enumerate(lines(read_capped(path))):
        # `str.strip()` rather than an ASCII strip: it is the closest thing here
        # to Rust's Unicode `str::trim`.
        line = raw.strip()
        if line == "" or line.startswith("#"):
            continue
        if count == 256:
            raise unreadable(
                path, f"more than 256 entries: a 257th appears on line {index + 1}"
            )
        entry = parse_entry(line)
        if entry is None:
            raise unreadable(
                path,
                f"line {index + 1} is not three integers in 0..=255: "
                f"{escape_debug(line)}",
            )
        table.extend(entry)
        count += 1

    if count != 256:
        raise unreadable(path, f"expected 256 entries, found {count}")
    return bytes(table)


def lut(pixels: bytearray, path: str) -> bytearray:
    """Apply a colour lookup table read from a file.

    This is the step with a capability cost. The plugin opens the file itself,
    so the host has to grant `fs.read` over a directory the file is in, and the
    grant is checked against the component's declared imports at *load* time --
    before a pixel is touched, and before this function has ever run.
    """
    table = load_lut(path)
    for at in range(0, len(pixels) - 2, CHANNELS):
        # Per channel, never across channels: red is looked up in the red column
        # of the entry the red input selects. Cross-channel mixing would make
        # this a matrix, and the file format is not one.
        pixels[at] = table[pixels[at] * CHANNELS]
        pixels[at + 1] = table[pixels[at + 1] * CHANNELS + 1]
        pixels[at + 2] = table[pixels[at + 2] * CHANNELS + 2]
    return pixels


# ---------------------------------------------------------------------------
# The world's exports
# ---------------------------------------------------------------------------


def report(failure: Failure) -> None:
    """Log a failure on its way out.

    The mirror of `report` in the Rust guest. Every case of `failure` carries
    its own reason, so this line is a courtesy to whoever is watching the host's
    log rather than the only channel, and there is exactly one of it per failed
    call.
    """
    if isinstance(failure, Failure_Malformed):
        detail = f"malformed input: {failure.value}"
    elif isinstance(failure, Failure_Unreadable):
        detail = (
            f"unreadable lookup table {failure.value.path}: {failure.value.reason}"
        )
    else:
        # Unreachable today -- this plugin implements every case -- and kept so
        # the guests stay line-for-line comparable. Spelled to match the WIT
        # case names, since that is what a reader of the world will look for.
        names = {
            OperationKind.GRAYSCALE: "grayscale",
            OperationKind.INVERT: "invert",
            OperationKind.GAIN: "gain",
            OperationKind.RESIZE: "resize",
            OperationKind.LUT: "lut",
        }
        detail = f"unsupported step: {names[failure.value]}"
    log.emit(Level.ERROR, detail)


class WitWorld(wit_world.WitWorld):
    """The exports of the `asset-plugin` world."""

    def describe(self) -> PluginInfo:
        # `supports` is `list<operation-kind>`, so naming a step no longer means
        # constructing one. There is nothing here for a host to know to ignore,
        # and nothing for four guests to invent differently.
        return PluginInfo(
            name="py-asset",
            supports=[
                OperationKind.GRAYSCALE,
                OperationKind.INVERT,
                OperationKind.GAIN,
                OperationKind.RESIZE,
                OperationKind.LUT,
            ],
        )

    def apply(self, input: Image, steps: List[Operation]) -> Image:
        # Calling into the host: an import crossing, and one of the things the
        # recorder captures. Deterministic on purpose -- no counts that depend
        # on anything but the arguments.
        log.emit(
            Level.INFO,
            f"apply: {input.width}x{input.height}, {len(steps)} step(s)",
        )

        width, height = input.width, input.height
        pixels = bytearray(input.pixels)

        try:
            validate(width, height, pixels)

            # "Steps apply in order. The whole call fails on the first step that
            # cannot be done" -- so a plain loop that raises, and a partly
            # transformed image never escapes.
            for step in steps:
                if isinstance(step, Operation_Grayscale):
                    pixels = grayscale(pixels)
                elif isinstance(step, Operation_Invert):
                    pixels = invert(pixels)
                elif isinstance(step, Operation_Gain):
                    pixels = apply_gain(pixels, step.value)
                elif isinstance(step, Operation_Resize):
                    width, height, pixels = resize(width, height, pixels, step.value)
                else:
                    assert isinstance(step, Operation_Lut)
                    pixels = lut(pixels, step.value)
        except Refusal as refusal:
            report(refusal.failure)
            # `Err` is what componentize-py lowers into the `err` arm. Anything
            # else raised out of this method is a bug in the plugin and reaches
            # the host as a trap, which is the right difference: "I cannot do
            # this" is an answer, a bug is not.
            raise Err(refusal.failure) from None

        return Image(width=width, height=height, pixels=bytes(pixels))
