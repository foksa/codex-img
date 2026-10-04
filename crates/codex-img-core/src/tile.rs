use image::{Rgba, RgbaImage};
const STEP: usize = 6;
/// `out[x] = image[(x + shift) % w]`: rolling by half the width moves the edges to the centre.
pub fn roll(image: &RgbaImage, shift: u32) -> RgbaImage {
    let w = image.width();
    RgbaImage::from_fn(w, image.height(), |x, y| *image.get_pixel((x + shift) % w, y))
}

/// Two copies of the tile side by side, cropped around where they meet (up to 900 px each way).
pub fn join_preview(tile: &RgbaImage) -> RgbaImage {
    let (w, h) = tile.dimensions();
    let half = (w / 2).min(900);
    RgbaImage::from_fn(2 * half, h, |x, y| *tile.get_pixel((w - half + x) % w, y))
}

pub struct Spliced {
    pub image: RgbaImage,
    /// The columns the repaired band spans at its widest, in the rolled image.
    pub band: (u32, u32),
}

/// Take the band around the centre from `edited` and the rest from `rolled`. The band is where the
/// edit clearly changed things; on each side, the cut follows the path of least disagreement
/// within a wide zone, so it goes around anything the edit redrew (a whole mountain) instead of
/// through it. The spliced pixels are shifted by the edit's tone offset from the original,
/// measured just outside each cut, so no step shows in smooth gradients.
pub fn splice(rolled: &RgbaImage, edited: &RgbaImage) -> Spliced {
    let (w, h) = rolled.dimensions();
    let (wu, hu) = (w as usize, h as usize);
    let cost: Vec<u32> = rolled
        .pixels()
        .zip(edited.pixels())
        .map(|(a, b)| (0..3).map(|c| u32::from(a.0[c].abs_diff(b.0[c]))).sum())
        .collect();

    // The band: columns around the centre whose mean change stands out from the median column.
    let columns: Vec<f64> = (0..wu).map(|x| (0..hu).map(|y| f64::from(cost[y * wu + x])).sum::<f64>() / hu as f64).collect();
    let smooth: Vec<f64> = (0..wu).map(|x| {
        let (lo, hi) = (x.saturating_sub(4), (x + 5).min(wu));
        columns[lo..hi].iter().sum::<f64>() / (hi - lo) as f64
    }).collect();
    let mut sorted = smooth.clone();
    sorted.sort_by(f64::total_cmp);
    let threshold = (3.0 * sorted[wu / 2]).max(30.0);
    let (mut a, mut b) = (wu / 2, wu / 2);
    while a > 0 && smooth[a - 1] > threshold {
        a -= 1;
    }
    while b + 1 < wu && smooth[b + 1] > threshold {
        b += 1;
    }
    let min_half = wu / 25;
    let (a, b) = (a.min(wu / 2 - min_half), b.max(wu / 2 + min_half));

    let zone = wu / 4;
    let left = best_path(&cost, wu, hu, a.saturating_sub(zone), wu / 2 - 2, a);
    let right = best_path(&cost, wu, hu, wu / 2 + 2, (b + zone).min(wu), b);

    let probe = (wu / 32).max(2);
    let offsets = |columns: &dyn Fn(usize) -> std::ops::Range<usize>| smooth_rows(&(0..hu).map(|y| tone_offset(rolled, edited, y, columns(y))).collect::<Vec<_>>());
    let left_offsets = offsets(&|y| left[y].saturating_sub(probe)..left[y]);
    let right_offsets = offsets(&|y| right[y]..(right[y] + probe).min(wu));

    let mut image = rolled.clone();
    for y in 0..hu {
        let span = (right[y] - left[y]) as f64;
        for x in left[y]..right[y] {
            let t = (x - left[y]) as f64 / span;
            let p = edited.get_pixel(x as u32, y as u32).0;
            let mut out = [0u8; 4];
            for c in 0..3 {
                let offset = left_offsets[y][c] * (1.0 - t) + right_offsets[y][c] * t;
                out[c] = (f64::from(p[c]) + offset).round().clamp(0.0, 255.0) as u8;
            }
            out[3] = p[3];
            image.put_pixel(x as u32, y as u32, Rgba(out));
        }
    }
    let band = (*left.iter().min().unwrap_or(&a) as u32, *right.iter().max().unwrap_or(&b) as u32);
    Spliced { image, band }
}

/// A vertical path (one x per row) within `[lo, hi)` crossing the least disagreement. Moving
/// sideways by d pixels also costs the pixels passed in that row, since the cut runs along them.
/// Each pixel also costs a quarter of its distance from `band`, the band's edge on this side
/// (nothing inside the band): where the edit and the original agree equally, the cut stays close
/// to the band and takes as little of the edit as it can.
fn best_path(cost: &[u32], w: usize, h: usize, lo: usize, hi: usize, band: usize) -> Vec<usize> {
    let n = hi - lo;
    let outside = |x: usize| if band < w / 2 { band.saturating_sub(x) } else { x.saturating_sub(band) };
    let bias: Vec<u64> = (lo..hi).map(|x| (outside(x) / 4) as u64).collect();
    let pixel = |y: usize, i: usize| u64::from(cost[y * w + lo + i]) + bias[i];
    let mut acc: Vec<u64> = (0..n).map(|i| pixel(0, i)).collect();
    let mut back = vec![0usize; n * h];
    let mut prefix = vec![0u64; n + 1];
    for y in 1..h {
        for i in 0..n {
            prefix[i + 1] = prefix[i] + pixel(y, i);
        }
        let mut next = vec![u64::MAX; n];
        for (i, slot) in next.iter_mut().enumerate() {
            let mut best = (u64::MAX, i);
            for src in i.saturating_sub(STEP)..(i + STEP + 1).min(n) {
                let total = acc[src] + prefix[src.max(i)] - prefix[src.min(i)];
                if total < best.0 {
                    best = (total, src);
                }
            }
            *slot = best.0 + pixel(y, i);
            back[y * n + i] = best.1;
        }
        acc = next;
    }
    let mut i = (0..n).min_by_key(|&i| acc[i]).unwrap_or(0);
    let mut path = vec![0; h];
    for y in (0..h).rev() {
        path[y] = lo + i;
        i = back[y * n + i];
    }
    path
}

/// Median of `original - edited` per channel over `columns` of row `y`.
fn tone_offset(rolled: &RgbaImage, edited: &RgbaImage, y: usize, columns: std::ops::Range<usize>) -> [f64; 3] {
    let mut out = [0.0; 3];
    if columns.is_empty() {
        return out;
    }
    for (c, slot) in out.iter_mut().enumerate() {
        let mut diffs: Vec<i32> = columns.clone().map(|x| i32::from(rolled.get_pixel(x as u32, y as u32).0[c]) - i32::from(edited.get_pixel(x as u32, y as u32).0[c])).collect();
        diffs.sort_unstable();
        *slot = f64::from(diffs[diffs.len() / 2]);
    }
    out
}

/// Box-average each channel over 31 rows, so row noise doesn't become stripes.
fn smooth_rows(rows: &[[f64; 3]]) -> Vec<[f64; 3]> {
    let n = rows.len();
    (0..n)
        .map(|y| {
            let (lo, hi) = (y.saturating_sub(15), (y + 16).min(n));
            let mut sum = [0.0; 3];
            for row in &rows[lo..hi] {
                for c in 0..3 {
                    sum[c] += row[c];
                }
            }
            sum.map(|s| s / (hi - lo) as f64)
        })
        .collect()
}

#[cfg(test)]
mod tests {
use super::*;
    #[test]
    fn rolls_and_rolls_back() {
        let image = RgbaImage::from_fn(10, 2, |x, y| Rgba([x as u8, y as u8, 0, 255]));
        let rolled = roll(&image, 5);
        assert_eq!(rolled.get_pixel(0, 0).0[0], 5);
        assert_eq!(roll(&rolled, 10 - 5), image);
        assert_eq!(join_preview(&image).width(), 10);
    }

    /// A 200x60 rolled panorama: a smooth gradient with a hard seam at the centre, and an edit
    /// that joined it with a wide "mountain" reaching past the centre band on the right, drawn a
    /// shade brighter overall, as the model does.
    fn scene() -> (RgbaImage, RgbaImage) {
        let rolled = RgbaImage::from_fn(200, 60, |x, y| {
            let seam = if x < 100 { 0 } else { 40 };
            Rgba([(60 + y) as u8, (80 + seam) as u8, 200, 255])
        });
        let edited = RgbaImage::from_fn(200, 60, |x, y| {
            let mountain = y > 30 && (x as i32 - 110).unsigned_abs() < (y - 30) * 2;
            let joined = (60 + y + 4) as u8;
            if mountain {
                Rgba([240, 240, 250, 255])
            } else if (90..110).contains(&x) {
                Rgba([joined, 100, 204, 255])
            } else {
                let p = rolled.get_pixel(x, y).0;
                Rgba([p[0] + 4, p[1] + 4, p[2] + 4, 255])
            }
        });
        (rolled, edited)
    }

    #[test]
    fn splices_the_repaired_band_around_what_the_edit_redrew() {
        let (rolled, edited) = scene();
        let spliced = splice(&rolled, &edited).image;
        // Far from the centre, the original stays exactly.
        for x in (0..40).chain(170..200) {
            for y in 0..60 {
                assert_eq!(spliced.get_pixel(x, y), rolled.get_pixel(x, y), "({x}, {y})");
            }
        }
        // The whole mountain comes from the edit, including the part past the band on the right.
        let mountain = |x: u32, y: u32| y > 30 && (x as i32 - 110).unsigned_abs() < (y - 30) * 2;
        for (x, y) in (0..200).flat_map(|x| (0..60).map(move |y| (x, y))).filter(|&(x, y)| mountain(x, y)) {
            let p = spliced.get_pixel(x, y).0;
            assert!(p[0] >= 230 && p[2] >= 240, "mountain pixel ({x}, {y}) was cut: {p:?}");
        }
        // The edit's +4 tone shift is taken out, so the band matches the original's tone.
        assert_eq!(spliced.get_pixel(100, 10).0, [70, 96, 200, 255]);
        // The hard seam at the centre is gone: no column of the top rows jumps by 40.
        for x in 1..200 {
            let (p, q) = (spliced.get_pixel(x - 1, 5).0, spliced.get_pixel(x, 5).0);
            assert!(p[1].abs_diff(q[1]) < 40, "step at x={x}");
        }
    }

}
