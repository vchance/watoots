//! A QOI decoder as a watoots plugin.
//!
//!   tools/build-plugins.sh rust-qoi
//!
//! QOI (the "Quite OK Image" format, qoiformat.org) is real, adopted, and fits
//! on one page: a 14-byte header, then a stream of chunks, then an 8-byte end
//! marker. It is here because it is small enough to implement in every guest
//! language and because its header carries the dimensions as two `u32`s, which
//! is exactly the field a hostile file lies in.
//!
//! On that: this decoder checks the arithmetic -- `width * height * 4` must
//! not overflow -- and *nothing else* about size. It does not know the host's
//! memory budget and should not guess. A header claiming a 1 GiB image is the
//! sandbox's to refuse, and `limits.memory` does, whether or not the plugin was
//! written carefully. Being able to say that is the reason this example exists.

wit_bindgen::generate!({
    path: "../../wit/preview",
    world: "decoder",
});

// `Image` and `Failure` arrive at the root via `use types.{image, failure}`
// in the world; importing them from the interface again would be a duplicate.

const MAGIC: &[u8; 4] = b"qoif";
const HEADER_LEN: usize = 14;
const END_MARKER: [u8; 8] = [0, 0, 0, 0, 0, 0, 0, 1];

struct Qoi;

impl Guest for Qoi {
    fn format() -> String {
        "qoi".to_string()
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

        let width = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
        let height = u32::from_be_bytes([data[8], data[9], data[10], data[11]]);
        let channels = data[12];
        if !(3..=4).contains(&channels) {
            return Err(Failure::Corrupt(format!("channels must be 3 or 4, not {channels}")));
        }
        if width == 0 || height == 0 {
            return Err(Failure::Corrupt("zero dimension".to_string()));
        }

        // Overflow is a correctness question and gets checked. Whether the
        // resulting size is *affordable* is a policy question, and is not.
        let pixel_count = (width as u64) * (height as u64);
        let byte_count = pixel_count
            .checked_mul(4)
            .filter(|&n| n <= usize::MAX as u64)
            .ok_or_else(|| Failure::Corrupt(format!("{width}x{height} overflows")))?;

        // This is the allocation a hostile header aims at. Under
        // `limits.memory` the sandbox refuses it here, and the call fails
        // without the host process having grown by a byte.
        let mut pixels: Vec<u8> = Vec::with_capacity(byte_count as usize);

        let mut index = [[0u8, 0, 0, 0]; 64];
        let mut px = [0u8, 0, 0, 255];
        let mut pos = HEADER_LEN;
        let body_end = data.len().saturating_sub(END_MARKER.len());

        while pixels.len() < byte_count as usize {
            if pos >= body_end {
                // Ran out of chunks before filling the image. Report how much
                // is missing in pixels' worth of the smallest chunk, which is
                // honest about the shape of the shortfall if not its exact size.
                let missing_pixels = (byte_count as usize - pixels.len()) / 4;
                return Err(Failure::Truncated(missing_pixels as u64));
            }
            let b1 = data[pos];
            pos += 1;

            match b1 {
                0xFE => {
                    // QOI_OP_RGB
                    if pos + 3 > body_end {
                        return Err(Failure::Truncated((pos + 3 - body_end) as u64));
                    }
                    px[0] = data[pos];
                    px[1] = data[pos + 1];
                    px[2] = data[pos + 2];
                    pos += 3;
                }
                0xFF => {
                    // QOI_OP_RGBA
                    if pos + 4 > body_end {
                        return Err(Failure::Truncated((pos + 4 - body_end) as u64));
                    }
                    px = [data[pos], data[pos + 1], data[pos + 2], data[pos + 3]];
                    pos += 4;
                }
                _ => match b1 >> 6 {
                    0b00 => {
                        // QOI_OP_INDEX
                        px = index[(b1 & 0x3F) as usize];
                    }
                    0b01 => {
                        // QOI_OP_DIFF: three 2-bit deltas, bias 2.
                        px[0] = px[0].wrapping_add(((b1 >> 4) & 0x03).wrapping_sub(2));
                        px[1] = px[1].wrapping_add(((b1 >> 2) & 0x03).wrapping_sub(2));
                        px[2] = px[2].wrapping_add((b1 & 0x03).wrapping_sub(2));
                    }
                    0b10 => {
                        // QOI_OP_LUMA: 6-bit green delta (bias 32), then a byte
                        // of 4-bit red/blue deltas relative to green (bias 8).
                        if pos >= body_end {
                            return Err(Failure::Truncated(1));
                        }
                        let b2 = data[pos];
                        pos += 1;
                        let dg = (b1 & 0x3F).wrapping_sub(32);
                        let dr_dg = (b2 >> 4).wrapping_sub(8);
                        let db_dg = (b2 & 0x0F).wrapping_sub(8);
                        px[0] = px[0].wrapping_add(dg.wrapping_add(dr_dg));
                        px[1] = px[1].wrapping_add(dg);
                        px[2] = px[2].wrapping_add(dg.wrapping_add(db_dg));
                    }
                    _ => {
                        // QOI_OP_RUN: repeat the previous pixel, bias 1.
                        let run = ((b1 & 0x3F) as usize) + 1;
                        for _ in 0..run {
                            if pixels.len() >= byte_count as usize {
                                break;
                            }
                            pixels.extend_from_slice(&px);
                        }
                        // The run already wrote the pixel; skip the shared push
                        // below by continuing, but still update the index.
                        index[hash(px)] = px;
                        continue;
                    }
                },
            }

            index[hash(px)] = px;
            pixels.extend_from_slice(&px);
        }

        // The end marker is part of the format. A file without one is not a
        // QOI file that happened to end early; it is a stream cut off mid-way.
        if data.len() < pos + END_MARKER.len() || data[data.len() - END_MARKER.len()..] != END_MARKER {
            return Err(Failure::Corrupt("missing end marker".to_string()));
        }

        Ok(Image {
            width,
            height,
            pixels,
        })
    }
}

fn hash(px: [u8; 4]) -> usize {
    ((px[0] as usize) * 3 + (px[1] as usize) * 5 + (px[2] as usize) * 7 + (px[3] as usize) * 11)
        % 64
}

export!(Qoi);
