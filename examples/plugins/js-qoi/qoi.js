// A QOI decoder as a watoots plugin, in JavaScript.
//
//   npm install && npm run build
//
// The same world as the Rust and C++ decoders, byte for byte on the same
// inputs. This one runs inside StarlingMonkey, a SpiderMonkey build, which is
// why its policy grants what a JS engine needs rather than what a decoder
// does -- the import list reflects the runtime, not the author, and a
// previewer that installs a JavaScript codec is granting a JavaScript engine.
//
// On the bomb: `new Uint8Array(1 GiB)` asks linear memory to grow. Under
// `limits.memory` the sandbox refuses, the engine cannot satisfy the request,
// and what reaches the host is a trap in a call where growth was refused --
// which watoots reports as the ceiling, not as a crash. The decoder does not
// check the size against a budget, because it does not have one to check
// against. That is the point.

const MAGIC = [0x71, 0x6f, 0x69, 0x66]; // "qoif"
const HEADER_LEN = 14;
const END_MARKER = [0, 0, 0, 0, 0, 0, 0, 1];

/**
 * A failure on its way out of `decode`.
 *
 * jco lowers a thrown value into the `err` arm when it carries an own `payload`
 * property; anything else that is an `Error` becomes a trap. So this is how
 * "not my format" is an answer rather than a crash.
 */
class Refusal extends Error {
  constructor(payload) {
    super();
    this.payload = payload;
  }
}

const notThisFormat = () => new Refusal({ tag: 'not-this-format' });
// `u64` crosses as a BigInt.
const truncated = (missing) => new Refusal({ tag: 'truncated', val: BigInt(missing) });
const corrupt = (why) => new Refusal({ tag: 'corrupt', val: why });

const hasMagic = (bytes) =>
  bytes.length >= MAGIC.length && MAGIC.every((b, i) => bytes[i] === b);

const readU32 = (bytes, at) =>
  ((bytes[at] << 24) | (bytes[at + 1] << 16) | (bytes[at + 2] << 8) | bytes[at + 3]) >>> 0;

const hash = (r, g, b, a) => (r * 3 + g * 5 + b * 7 + a * 11) % 64;

export function format() {
  return 'qoi';
}

export function sniff(prefix) {
  return hasMagic(prefix);
}

export function decode(data) {
  if (!hasMagic(data)) {
    throw notThisFormat();
  }
  if (data.length < HEADER_LEN) {
    throw truncated(HEADER_LEN - data.length);
  }

  const width = readU32(data, 4);
  const height = readU32(data, 8);
  const channels = data[12];
  if (channels < 3 || channels > 4) {
    throw corrupt(`channels must be 3 or 4, not ${channels}`);
  }
  if (width === 0 || height === 0) {
    throw corrupt('zero dimension');
  }

  // Overflow is a correctness question. Affordability is a policy question,
  // and is left to the policy.
  const pixelCount = width * height;
  const byteCount = pixelCount * 4;
  if (!Number.isSafeInteger(byteCount) || byteCount > 0xffffffff) {
    throw corrupt(`${width}x${height} overflows`);
  }

  // The allocation a hostile header aims at.
  const pixels = new Uint8Array(byteCount);

  const index = new Uint8Array(64 * 4);
  let r = 0, g = 0, b = 0, a = 255;
  let pos = HEADER_LEN;
  let written = 0;
  const bodyEnd = Math.max(0, data.length - END_MARKER.length);

  const emit = () => {
    pixels[written++] = r;
    pixels[written++] = g;
    pixels[written++] = b;
    pixels[written++] = a;
  };
  const remember = () => {
    const at = hash(r, g, b, a) * 4;
    index[at] = r;
    index[at + 1] = g;
    index[at + 2] = b;
    index[at + 3] = a;
  };

  while (written < byteCount) {
    if (pos >= bodyEnd) {
      throw truncated((byteCount - written) / 4);
    }
    const b1 = data[pos++];

    if (b1 === 0xfe) {
      // QOI_OP_RGB
      if (pos + 3 > bodyEnd) throw truncated(pos + 3 - bodyEnd);
      r = data[pos]; g = data[pos + 1]; b = data[pos + 2];
      pos += 3;
    } else if (b1 === 0xff) {
      // QOI_OP_RGBA
      if (pos + 4 > bodyEnd) throw truncated(pos + 4 - bodyEnd);
      r = data[pos]; g = data[pos + 1]; b = data[pos + 2]; a = data[pos + 3];
      pos += 4;
    } else {
      switch (b1 >> 6) {
        case 0: {
          // QOI_OP_INDEX
          const at = (b1 & 0x3f) * 4;
          r = index[at]; g = index[at + 1]; b = index[at + 2]; a = index[at + 3];
          break;
        }
        case 1:
          // QOI_OP_DIFF: three 2-bit deltas, bias 2
          r = (r + ((b1 >> 4) & 0x03) - 2) & 0xff;
          g = (g + ((b1 >> 2) & 0x03) - 2) & 0xff;
          b = (b + (b1 & 0x03) - 2) & 0xff;
          break;
        case 2: {
          // QOI_OP_LUMA: 6-bit green delta, then 4+4 red/blue relative to green
          if (pos >= bodyEnd) throw truncated(1);
          const b2 = data[pos++];
          const dg = (b1 & 0x3f) - 32;
          const drDg = (b2 >> 4) - 8;
          const dbDg = (b2 & 0x0f) - 8;
          r = (r + dg + drDg) & 0xff;
          g = (g + dg) & 0xff;
          b = (b + dg + dbDg) & 0xff;
          break;
        }
        default: {
          // QOI_OP_RUN: repeat the previous pixel, bias 1
          const run = (b1 & 0x3f) + 1;
          for (let i = 0; i < run && written < byteCount; i++) emit();
          remember();
          continue;
        }
      }
    }

    remember();
    emit();
  }

  const tail = data.length - END_MARKER.length;
  if (data.length < pos + END_MARKER.length ||
      !END_MARKER.every((v, i) => data[tail + i] === v)) {
    throw corrupt('missing end marker');
  }

  return { width, height, pixels };
}
