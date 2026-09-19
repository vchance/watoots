//! A farbfeld decoder as a watoots plugin.
//!
//!   tools/build-plugins.sh rust-farbfeld
//!
//! farbfeld (tools.suckless.org/farbfeld) is the simplest real image format
//! there is: eight magic bytes, width and height as big-endian `u32`s, then
//! `width * height` pixels of 16-bit big-endian RGBA. It is here to be the
//! *second* format, so that a previewer with two decoders installed is
//! dispatching between two codecs rather than two copies of one.
//!
//! It also makes a point the QOI decoder cannot. farbfeld has no variable-
//! length chunks, so the file's size is a function of its header: a header
//! claiming 16384 x 16384 in a 1 KB file is *provably* truncated before a byte
//! of pixels is allocated, and this decoder says so. QOI's chunk stream means
//! the same lie cannot be caught up front -- the decoder has to allocate and
//! start reading -- which is why the QOI bomb is the sandbox's to stop and
//! this format's bomb is the decoder's. Both are fine outcomes. The difference
//! is that only one of them was in the plugin author's hands.

wit_bindgen::generate!({
    path: "../../wit/preview",
    world: "decoder",
});

// `Image` and `Failure` arrive at the root via `use types.{image, failure}`.

const MAGIC: &[u8; 8] = b"farbfeld";
const HEADER_LEN: usize = 16;
const BYTES_PER_PIXEL: u64 = 8; // four channels, 16 bits each

struct Farbfeld;

impl Guest for Farbfeld {
    fn format() -> String {
        "farbfeld".to_string()
    }

    fn sniff(prefix: Vec<u8>) -> bool {
        prefix.len() >= MAGIC.len() && &prefix[..MAGIC.len()] == MAGIC
    }

    fn decode(data: Vec<u8>) -> Result<Image, Failure> {
        if data.len() < MAGIC.len() || &data[..MAGIC.len()] != MAGIC {
            return Err(Failure::NotThisFormat);
        }
        if data.len() < HEADER_LEN {
            return Err(Failure::Truncated((HEADER_LEN - data.len()) as u64));
        }
        let width = u32::from_be_bytes([data[8], data[9], data[10], data[11]]);
        let height = u32::from_be_bytes([data[12], data[13], data[14], data[15]]);
        if width == 0 || height == 0 {
            return Err(Failure::Corrupt("zero dimension".to_string()));
        }

        // The whole file's length follows from the header, so a size lie is
        // caught here, before any allocation. Compare the QOI decoder, which
        // cannot know until it has read the chunks.
        let pixel_count = (width as u64) * (height as u64);
        let needed = pixel_count
            .checked_mul(BYTES_PER_PIXEL)
            .and_then(|n| n.checked_add(HEADER_LEN as u64))
            .ok_or_else(|| Failure::Corrupt(format!("{width}x{height} overflows")))?;
        let have = data.len() as u64;
        if have < needed {
            return Err(Failure::Truncated(needed - have));
        }

        // 16-bit to 8-bit: the high byte. Exact for anything that was 8-bit
        // to begin with and expanded as `b << 8 | b`, which is how the fixture
        // was made and how most 8-bit sources are widened.
        let mut pixels = Vec::with_capacity((pixel_count * 4) as usize);
        let body = &data[HEADER_LEN..needed as usize];
        for sample in body.chunks_exact(2) {
            pixels.push(sample[0]);
        }

        Ok(Image {
            width,
            height,
            pixels,
        })
    }
}

export!(Farbfeld);
