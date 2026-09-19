"""A QOI decoder as a watoots plugin, in Python.

    python3 -m venv .venv && ./.venv/bin/pip install componentize-py
    ./.venv/bin/componentize-py -d ../../wit/preview -w decoder \
        componentize app -o py_qoi.wasm

The same world as the Rust, C++ and JavaScript decoders, byte for byte on the
same inputs. This one runs inside CPython, which is why its policy grants what
an interpreter needs -- sockets, random, a wall clock, the environment -- rather
than what a decoder does. The import list reflects the runtime, not the author.

On the bomb: `bytearray(1 GiB)` asks linear memory to grow. Under
`limits.memory` the sandbox refuses; CPython raises `MemoryError`, which
componentize-py turns into a trap; and a trap in a call where growth was
refused is reported by watoots as the ceiling. The decoder checks the
arithmetic and nothing else about size, because it has no budget to check
against.
"""

from componentize_py_types import Err

import wit_world
from wit_world.imports.types import (
    Failure_Corrupt,
    Failure_NotThisFormat,
    Failure_Truncated,
    Image,
)

MAGIC = b"qoif"
HEADER_LEN = 14
END_MARKER = bytes([0, 0, 0, 0, 0, 0, 0, 1])


def _hash(r: int, g: int, b: int, a: int) -> int:
    return (r * 3 + g * 5 + b * 7 + a * 11) % 64


class WitWorld(wit_world.WitWorld):
    """The exports of the `decoder` world."""

    def format(self) -> str:
        return "qoi"

    def sniff(self, prefix: bytes) -> bool:
        return prefix[: len(MAGIC)] == MAGIC

    def decode(self, data: bytes) -> Image:
        if data[: len(MAGIC)] != MAGIC:
            raise Err(Failure_NotThisFormat())
        if len(data) < HEADER_LEN:
            raise Err(Failure_Truncated(HEADER_LEN - len(data)))

        width = int.from_bytes(data[4:8], "big")
        height = int.from_bytes(data[8:12], "big")
        channels = data[12]
        if channels not in (3, 4):
            raise Err(Failure_Corrupt(f"channels must be 3 or 4, not {channels}"))
        if width == 0 or height == 0:
            raise Err(Failure_Corrupt("zero dimension"))

        # Overflow is a correctness question. Affordability is the policy's.
        byte_count = width * height * 4
        if byte_count > 0xFFFFFFFF:
            raise Err(Failure_Corrupt(f"{width}x{height} overflows"))

        # The allocation a hostile header aims at.
        pixels = bytearray(byte_count)

        index = [(0, 0, 0, 0)] * 64
        r, g, b, a = 0, 0, 0, 255
        pos = HEADER_LEN
        written = 0
        body_end = max(0, len(data) - len(END_MARKER))

        while written < byte_count:
            if pos >= body_end:
                raise Err(Failure_Truncated((byte_count - written) // 4))
            b1 = data[pos]
            pos += 1

            if b1 == 0xFE:  # QOI_OP_RGB
                if pos + 3 > body_end:
                    raise Err(Failure_Truncated(pos + 3 - body_end))
                r, g, b = data[pos], data[pos + 1], data[pos + 2]
                pos += 3
            elif b1 == 0xFF:  # QOI_OP_RGBA
                if pos + 4 > body_end:
                    raise Err(Failure_Truncated(pos + 4 - body_end))
                r, g, b, a = data[pos], data[pos + 1], data[pos + 2], data[pos + 3]
                pos += 4
            else:
                tag = b1 >> 6
                if tag == 0:  # QOI_OP_INDEX
                    r, g, b, a = index[b1 & 0x3F]
                elif tag == 1:  # QOI_OP_DIFF: three 2-bit deltas, bias 2
                    r = (r + ((b1 >> 4) & 0x03) - 2) & 0xFF
                    g = (g + ((b1 >> 2) & 0x03) - 2) & 0xFF
                    b = (b + (b1 & 0x03) - 2) & 0xFF
                elif tag == 2:  # QOI_OP_LUMA: 6-bit green, then 4+4 red/blue vs green
                    if pos >= body_end:
                        raise Err(Failure_Truncated(1))
                    b2 = data[pos]
                    pos += 1
                    dg = (b1 & 0x3F) - 32
                    r = (r + dg + (b2 >> 4) - 8) & 0xFF
                    g = (g + dg) & 0xFF
                    b = (b + dg + (b2 & 0x0F) - 8) & 0xFF
                else:  # QOI_OP_RUN: repeat the previous pixel, bias 1
                    run = (b1 & 0x3F) + 1
                    px = bytes((r, g, b, a))
                    while run > 0 and written < byte_count:
                        pixels[written : written + 4] = px
                        written += 4
                        run -= 1
                    index[_hash(r, g, b, a)] = (r, g, b, a)
                    continue

            index[_hash(r, g, b, a)] = (r, g, b, a)
            pixels[written : written + 4] = bytes((r, g, b, a))
            written += 4

        if len(data) < pos + len(END_MARKER) or data[-len(END_MARKER):] != END_MARKER:
            raise Err(Failure_Corrupt("missing end marker"))

        return Image(width=width, height=height, pixels=bytes(pixels))
