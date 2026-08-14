// SPDX-License-Identifier: MIT
//
// Turns a captured frame into one number: how bright the scene looks, weighted
// toward the middle of the view. Deliberately free of any Windows type so the
// part that can actually be wrong is testable on any platform.
//
// Two things here are easy to get wrong and are the reason this module exists:
//   - sRGB bytes are gamma-encoded. Averaging them directly and calling the
//     result brightness is wrong: mid-gray (128) is ~21% of the light of white,
//     not 50%. Every sample is converted to linear light before averaging.
//   - captured rows are padded. The GPU hands back rows of `row_pitch` bytes,
//     which is >= width * 4; reading width * 4 * y as the row start drifts into
//     garbage on any frame whose width is not a nice multiple.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};

/// One captured frame: 8-bit BGRA, top-left origin, rows padded to `row_pitch`.
pub struct Frame<'a> {
    pub pixels: &'a [u8],
    pub width: u32,
    pub height: u32,
    /// Bytes per row, padding included. Never less than `width * 4`.
    pub row_pitch: u32,
}

/// Fixed sampling grid. Resolution-independent on purpose: the same scene reads
/// the same whether VRChat runs at 1080p or 4K, and the cost never grows with
/// window size. 64x36 keeps the aspect of a typical window and is far more
/// signal than a pupil needs.
const GRID_X: u32 = 64;
const GRID_Y: u32 = 36;

/// Spread of the centre weighting, in units of half-width. At 0.5 a corner keeps
/// ~2% of the centre's weight — present, but clearly peripheral.
const SIGMA: f32 = 0.5;

/// sRGB byte -> linear light, precomputed once.
fn srgb_to_linear() -> &'static [f32; 256] {
    static TABLE: OnceLock<[f32; 256]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = [0.0f32; 256];
        for (i, slot) in table.iter_mut().enumerate() {
            let c = i as f32 / 255.0;
            *slot = if c <= 0.040_45 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            };
        }
        table
    })
}

/// Weight of a sample at normalised position (0..1, 0..1) within the frame.
/// `center_weight` 0 is a flat average of the whole view; 1 is a pure gaussian
/// on the middle. Anything in between keeps the periphery contributing, which is
/// what we want: the eye reacts to the whole field, just not equally.
fn weight_at(u: f32, v: f32, center_weight: f32) -> f32 {
    let nx = (u - 0.5) * 2.0;
    let ny = (v - 0.5) * 2.0;
    let d2 = nx * nx + ny * ny;
    let gaussian = (-d2 / (2.0 * SIGMA * SIGMA)).exp();
    (1.0 - center_weight) + center_weight * gaussian
}

/// Mean linear luminance over the frame, weighted toward the centre.
/// Returns 0 for a degenerate frame rather than failing: a missing measurement
/// should read as darkness, not crash the bridge.
pub fn weighted_luminance(frame: &Frame, center_weight: f32) -> f32 {
    if frame.width == 0 || frame.height == 0 {
        return 0.0;
    }
    let table = srgb_to_linear();
    let center_weight = center_weight.clamp(0.0, 1.0);
    let pitch = frame.row_pitch.max(frame.width * 4) as usize;

    let mut sum = 0.0f32;
    let mut total_weight = 0.0f32;

    for gy in 0..GRID_Y {
        // Sample cell centres, so the grid never lands on the very edge.
        let v = (gy as f32 + 0.5) / GRID_Y as f32;
        let y = ((v * frame.height as f32) as u32).min(frame.height - 1) as usize;

        for gx in 0..GRID_X {
            let u = (gx as f32 + 0.5) / GRID_X as f32;
            let x = ((u * frame.width as f32) as u32).min(frame.width - 1) as usize;

            let offset = y * pitch + x * 4;
            let Some(px) = frame.pixels.get(offset..offset + 4) else {
                continue;
            };

            // BGRA, and Rec.709 weights on linear light — green carries most of
            // what the eye calls brightness.
            let linear = 0.2126 * table[px[2] as usize]
                + 0.7152 * table[px[1] as usize]
                + 0.0722 * table[px[0] as usize];

            let w = weight_at(u, v, center_weight);
            sum += linear * w;
            total_weight += w;
        }
    }

    if total_weight > 0.0 {
        sum / total_weight
    } else {
        0.0
    }
}

/// The latest reading, shared between the capture callback and the loop that
/// drives the pupil. A slot rather than a queue on purpose: if the loop is late,
/// it wants the newest measurement, never a backlog of stale ones. Lock-free so
/// the capture callback never blocks on the consumer.
#[derive(Clone, Debug)]
pub struct LatestLuminance(Arc<AtomicU32>);

impl LatestLuminance {
    pub fn new() -> Self {
        Self(Arc::new(AtomicU32::new(0f32.to_bits())))
    }

    pub fn set(&self, value: f32) {
        self.0.store(value.to_bits(), Ordering::Relaxed);
    }

    pub fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Ordering::Relaxed))
    }
}

impl Default for LatestLuminance {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a BGRA frame from a per-pixel closure, with optional row padding
    /// filled with a value that must never reach the result.
    fn frame_of(
        width: u32,
        height: u32,
        padding: u32,
        f: impl Fn(u32, u32) -> [u8; 4],
    ) -> (Vec<u8>, u32) {
        let pitch = width * 4 + padding;
        let mut buf = vec![0xAAu8; (pitch * height) as usize];
        for y in 0..height {
            for x in 0..width {
                let o = (y * pitch + x * 4) as usize;
                buf[o..o + 4].copy_from_slice(&f(x, y));
            }
        }
        (buf, pitch)
    }

    fn measure(pixels: &[u8], width: u32, height: u32, pitch: u32, cw: f32) -> f32 {
        weighted_luminance(
            &Frame {
                pixels,
                width,
                height,
                row_pitch: pitch,
            },
            cw,
        )
    }

    #[test]
    fn black_reads_zero_and_white_reads_one() {
        let (black, p) = frame_of(64, 64, 0, |_, _| [0, 0, 0, 255]);
        assert!(measure(&black, 64, 64, p, 0.5) < 1e-6);

        let (white, p) = frame_of(64, 64, 0, |_, _| [255, 255, 255, 255]);
        assert!((measure(&white, 64, 64, p, 0.5) - 1.0).abs() < 1e-3);
    }

    #[test]
    fn mid_gray_is_a_fifth_of_the_light_not_half() {
        // The whole point of converting to linear: sRGB 128 emits ~21.6% of the
        // light of white. Averaging the raw bytes would say 50%.
        let (gray, p) = frame_of(64, 64, 0, |_, _| [128, 128, 128, 255]);
        let l = measure(&gray, 64, 64, p, 0.0);
        assert!((l - 0.2159).abs() < 0.005, "got {l}, expected ~0.216");
    }

    #[test]
    fn row_padding_is_skipped() {
        // Same image, once tight and once with 37 bytes of junk per row. A frame
        // that read the padding would come out very different.
        let (tight, p1) = frame_of(50, 20, 0, |x, _| [(x * 5) as u8; 4]);
        let (padded, p2) = frame_of(50, 20, 37, |x, _| [(x * 5) as u8; 4]);
        let a = measure(&tight, 50, 20, p1, 0.4);
        let b = measure(&padded, 50, 20, p2, 0.4);
        assert!((a - b).abs() < 1e-6, "tight {a} vs padded {b}");
    }

    /// Bright disc in the middle, dark surround — and its exact inverse.
    fn center_bright(x: u32, y: u32, w: u32, h: u32) -> bool {
        let nx = x as f32 / w as f32 - 0.5;
        let ny = y as f32 / h as f32 - 0.5;
        (nx * nx + ny * ny).sqrt() < 0.25
    }

    #[test]
    fn centre_weighting_favours_the_middle() {
        let (bright_mid, p) = frame_of(128, 128, 0, |x, y| {
            if center_bright(x, y, 128, 128) {
                [255, 255, 255, 255]
            } else {
                [0, 0, 0, 255]
            }
        });
        let (dark_mid, _) = frame_of(128, 128, 0, |x, y| {
            if center_bright(x, y, 128, 128) {
                [0, 0, 0, 255]
            } else {
                [255, 255, 255, 255]
            }
        });

        let flat_a = measure(&bright_mid, 128, 128, p, 0.0);
        let flat_b = measure(&dark_mid, 128, 128, p, 0.0);
        let weighted_a = measure(&bright_mid, 128, 128, p, 0.8);
        let weighted_b = measure(&dark_mid, 128, 128, p, 0.8);

        // The disc is only ~20% of the frame, so centre weighting cannot make it
        // outvote the surround — nor should it, since the periphery is meant to
        // keep counting. What it must do is pull the reading toward the middle,
        // in both directions.
        assert!(
            weighted_a > flat_a,
            "a bright middle must read brighter once weighted: {flat_a} -> {weighted_a}"
        );
        assert!(
            weighted_b < flat_b,
            "a dark middle must read darker once weighted: {flat_b} -> {weighted_b}"
        );
    }

    #[test]
    fn a_flat_weighting_is_a_plain_average() {
        // center_weight = 0 must fall back to counting every sample equally, so
        // the knob genuinely spans "whole view" to "mostly the middle".
        let (img, p) = frame_of(64, 64, 0, |x, y| {
            if (x + y) % 2 == 0 {
                [255, 255, 255, 255]
            } else {
                [0, 0, 0, 255]
            }
        });
        let flat = measure(&img, 64, 64, p, 0.0);
        assert!(
            (flat - 0.5).abs() < 0.02,
            "checkerboard should average ~0.5, got {flat}"
        );
    }

    #[test]
    fn measurement_is_resolution_independent() {
        // Same gradient at two sizes must land on nearly the same number.
        let (small, p1) = frame_of(160, 90, 0, |x, _| [(x * 255 / 159) as u8; 4]);
        let (large, p2) = frame_of(1600, 900, 0, |x, _| [(x * 255 / 1599) as u8; 4]);
        let a = measure(&small, 160, 90, p1, 0.6);
        let b = measure(&large, 1600, 900, p2, 0.6);
        assert!((a - b).abs() < 0.01, "{a} vs {b}");
    }

    #[test]
    fn the_shared_slot_always_holds_the_newest_reading() {
        let slot = LatestLuminance::new();
        assert_eq!(slot.get(), 0.0);

        let writer = slot.clone();
        std::thread::spawn(move || {
            for i in 0..1000 {
                writer.set(i as f32 / 1000.0);
            }
        })
        .join()
        .unwrap();

        // Whatever interleaving happened, the reader sees the last write, not a
        // queued-up backlog.
        assert!((slot.get() - 0.999).abs() < 1e-6);
    }

    #[test]
    fn degenerate_frames_read_as_dark() {
        assert_eq!(measure(&[], 0, 0, 0, 0.5), 0.0);
        // A buffer shorter than it claims must not panic.
        assert_eq!(measure(&[0, 0, 0, 255], 64, 64, 256, 0.5), 0.0);
    }
}
