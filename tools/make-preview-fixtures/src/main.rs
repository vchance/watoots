// Produce the preview fixtures with the reference `qoi` crate, so the valid
// image comes from an implementation that is not ours.
use std::fs;

fn main() {
    let out = std::env::args().nth(1).expect("output dir");
    fs::create_dir_all(&out).unwrap();

    // 32x32 RGBA designed to exercise every QOI op: solid blocks (RUN),
    // smooth ramps (DIFF, LUMA), revisited colours (INDEX), an alpha change
    // (RGBA) and a big jump (RGB).
    let (w, h) = (32u32, 32u32);
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let (r, g, b, a): (u8, u8, u8, u8) = match y / 8 {
                // Solid: RUN.
                0 => (200, 30, 30, 255),
                // Gentle ramp, +1 red per pixel: DIFF (deltas within -2..1).
                1 => ((x + y) as u8, 100, 50, 255),
                // Green-led ramp, +5 green with red/blue following closely:
                // LUMA (dg within -32..31, dr-dg and db-dg within -8..7).
                2 => ((x * 5 + 3) as u8, (x * 5) as u8, (x * 5 + 1) as u8, 255),
                // Revisited colours from row block 0 and 1 (INDEX), a big jump
                // (RGB), and a varying alpha (RGBA).
                _ => match x % 4 {
                    0 => (200, 30, 30, 255),
                    1 => (10, 240, 90, ((x * 7 + y * 3) % 256) as u8),
                    2 => (33, 100, 50, 255),
                    _ => (255, 0, 128, 255),
                },
            };
            px.extend_from_slice(&[r, g, b, a]);
        }
    }
    let encoded = qoi::encode_to_vec(&px, w, h).unwrap();
    fs::write(format!("{out}/blocks.qoi"), &encoded).unwrap();
    fs::write(format!("{out}/blocks.rgba"), &px).unwrap();

    // The bomb: a valid file's header with the dimensions replaced. 16384 x
    // 16384 x 4 = 1 GiB, which fits in u32 (so the decoder's overflow check
    // passes -- this is not a corrupt file) and is far beyond any sane
    // `limits.memory`. The body is the real image's body, so nothing else about
    // the file is wrong: only the claim.
    let mut bomb = encoded.clone();
    bomb[4..8].copy_from_slice(&16384u32.to_be_bytes());
    bomb[8..12].copy_from_slice(&16384u32.to_be_bytes());
    fs::write(format!("{out}/bomb.qoi"), &bomb).unwrap();

    // Cut off mid-stream: header intact, half the chunks, no end marker.
    let cut = &encoded[..encoded.len() / 2];
    fs::write(format!("{out}/truncated.qoi"), cut).unwrap();

    // Right length, wrong magic: what a renamed file looks like.
    let mut other = encoded.clone();
    other[..4].copy_from_slice(b"RIFF");
    fs::write(format!("{out}/not-qoi.bin"), &other).unwrap();

    println!("blocks.qoi {} bytes, bomb.qoi {} bytes, truncated.qoi {} bytes",
        encoded.len(), bomb.len(), cut.len());
}
