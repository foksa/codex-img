//! Pixel-grid detection and snapping: find the true cell grid of "pixel art" whose art pixels are
//! several image pixels wide, unevenly placed and soft-edged (generated or resampled art), and
//! reduce it to one pixel per cell. Algorithm: `docs/research/pixel-grid.md` §4.
//!
//! Per axis an edge profile gives candidate periods (FFT fundamental and autocorrelation lag); each
//! candidate gets an elastic boundary fit (DP); the period is the median of the candidates whose
//! noise-floor-corrected edge capture is near the best, refitted once at the fit's own mean cell
//! width. A confident uniform FFT grid at that period competes with the elastic fit on cell purity.
//! Cells take a tolerant-mode colour and a hardened alpha. Weak capture, impure cells or too few
//! cells refuse: the image is not pixel art. `MIN_CAPTURE` and `MIN_PURITY` were calibrated on 30
//! real generated images (20 pixel art, the rest watercolours, paintings, photos, icons and
//! renders): pixel art had capture ≥ 0.194 and purity ≥ 0.902. Smooth flat vector art and JPEG
//! block grids can still pass (capture 0.2–0.3, purity ≥ 0.94).
//!
//! `edge_profiles`, `estimate_axis`, `phase_and_coherence`, `dft`, `Spectrum`, `fft`, `peak_lag`,
//! `edge_strength`, `capture` and `fit_boundaries` are ported from unfake.js
//! (<https://github.com/jenissimo/unfake.js>, `crates/unfake-core/src/detect/{grid,edge,elastic}.rs`),
//! with changes marked where they occur. Their licence:
//!
//! MIT License
//!
//! Copyright (c) 2024 unfake.js team
//!
//! Permission is hereby granted, free of charge, to any person obtaining a copy
//! of this software and associated documentation files (the "Software"), to deal
//! in the Software without restriction, including without limitation the rights
//! to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
//! copies of the Software, and to permit persons to whom the Software is
//! furnished to do so, subject to the following conditions:
//!
//! The above copyright notice and this permission notice shall be included in all
//! copies or substantial portions of the Software.
//!
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
//! IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
//! FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
//! AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
//! LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
//! OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
//! SOFTWARE.
use image::{Rgba, RgbaImage};
use serde_json::{json, Value};
use std::f64::consts::PI;
use std::fmt;

/// Smallest cell size searched by default, in image px (real sheets have 4.7 px cells).
pub const DEFAULT_MIN_CELL: f32 = 4.0;
/// Fewest cells on the short axis: the largest period searched is `min(W, H) / MIN_CELLS`,
/// which also keeps sprite-sheet repeat periods out, and a grid with fewer cells refuses.
pub const MIN_CELLS: u32 = 16;
/// Refuse below this edge capture (share of above-noise edge energy on the boundaries). Real pixel
/// art measured 0.194–0.52, non-pixel art mostly 0.01–0.15.
pub const MIN_CAPTURE: f32 = 0.17;
/// Refuse below this cell purity (mean share of a cell's pixels near its colour). Real pixel art
/// measured 0.90–1.0; watercolours and photos 0.55–0.84.
pub const MIN_PURITY: f32 = 0.85;

/// Pixels within this Euclidean RGB distance count as the same colour (tolerant mode, purity).
const COLOUR_TOLERANCE: i32 = 28;
/// Most pixels the tolerant-mode search compares pairwise per cell.
const MODE_SAMPLE: usize = 256;
/// Candidates whose capture is within this share of the best one are kept.
const KEEP_SHARE: f64 = 0.95;
/// A kept candidate this many times smaller than the largest kept one is a sub-multiple.
const SUBMULTIPLE_RATIO: f64 = 1.6;
/// Both FFT axes need this coherence for the regular grid to compete.
const REGULAR_COHERENCE: f64 = 0.15;
/// FFT axes must agree with each other and with the chosen period within this share.
const AXIS_AGREEMENT: f64 = 0.02;
/// Refit at the fitted mean cell width when it differs from the chosen period by more than this.
const REFIT_SHARE: f64 = 0.003;
/// The elastic fit replaces the regular grid only when its purity is higher by more than this.
const ELASTIC_PURITY_MARGIN: f64 = 0.002;

/// What `detect` searches for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DetectOptions {
    /// A fixed (possibly fractional) cell size: no period search and no refusal on confidence.
    pub size: Option<f32>,
    /// Smallest cell size searched.
    pub min_cell: f32,
}

impl Default for DetectOptions {
    fn default() -> Self {
        DetectOptions { size: None, min_cell: DEFAULT_MIN_CELL }
    }
}

/// How the boundaries were placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// `offset + i · period` on each axis, from the FFT phase.
    Regular,
    /// Fitted to the drawn edges, each cell 0.75–1.3 × the period.
    Elastic,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Mode::Regular => "regular",
            Mode::Elastic => "elastic",
        }
    }
}

/// A detected cell grid and how confident the detection is.
#[derive(Debug, Clone, PartialEq)]
pub struct Grid {
    /// Mean cell size per axis `[x, y]`, in image px.
    pub period: [f32; 2],
    /// First boundary per axis (may be negative: the first cell can start outside the image).
    pub offset: [f32; 2],
    pub mode: Mode,
    /// Cell boundaries in image px, fractional and ascending; cell `i` spans `bx[i]..bx[i + 1]`.
    /// The outer two may lie outside the image; every cell's centre lies inside it.
    pub bx: Vec<f32>,
    pub by: Vec<f32>,
    /// Cells per axis: `bx.len() - 1`, `by.len() - 1`.
    pub cells: [u32; 2],
    /// Share of the above-noise edge energy that lies on the boundaries (mean of both axes).
    pub capture: f32,
    /// Mean share of an opaque cell's inner pixels within the colour tolerance of its colour.
    pub purity: f32,
    /// Share of each axis's edge energy aligned with a regular grid of its period.
    pub coherence: [f32; 2],
    /// Candidate periods that were compared (empty for a fixed size).
    pub candidates: Vec<f32>,
}

impl Grid {
    pub fn to_json(&self) -> Value {
        json!({
            "period": self.period.map(round4),
            "offset": self.offset.map(round4),
            "mode": self.mode.name(),
            "cells": self.cells,
            "capture": round4(self.capture),
            "purity": round4(self.purity),
            "coherence": self.coherence.map(round4),
            "candidates": self.candidates.iter().copied().map(round4).collect::<Vec<_>>(),
            "bx": self.bx.iter().copied().map(round4).collect::<Vec<_>>(),
            "by": self.by.iter().copied().map(round4).collect::<Vec<_>>(),
        })
    }
}

/// Why `detect` found no grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalReason {
    /// No candidate period in `[min_cell, min(W, H) / MIN_CELLS]` (or the image is too small).
    NoPeriod,
    /// The fixed size is not a usable cell size for this image.
    InvalidSize,
    LowCapture,
    LowPurity,
    TooFewCells,
}

impl RefusalReason {
    pub fn name(self) -> &'static str {
        match self {
            RefusalReason::NoPeriod => "no_period",
            RefusalReason::InvalidSize => "invalid_size",
            RefusalReason::LowCapture => "low_capture",
            RefusalReason::LowPurity => "low_purity",
            RefusalReason::TooFewCells => "too_few_cells",
        }
    }
}

/// No pixel grid: the reasons, and the measurements of the best grid found when there was one.
#[derive(Debug, Clone, PartialEq)]
pub struct Refusal {
    pub reasons: Vec<RefusalReason>,
    pub grid: Option<Box<Grid>>,
}

impl Refusal {
    fn bare(reason: RefusalReason) -> Self {
        Refusal { reasons: vec![reason], grid: None }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "refused": true,
            "reasons": self.reasons.iter().map(|r| r.name()).collect::<Vec<_>>(),
            "measurements": self.grid.as_deref().map(Grid::to_json),
        })
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts = Vec::new();
        for reason in &self.reasons {
            parts.push(match (reason, self.grid.as_deref()) {
                (RefusalReason::LowCapture, Some(g)) => format!("edge capture {:.3} < {MIN_CAPTURE}", g.capture),
                (RefusalReason::LowPurity, Some(g)) => format!("cell purity {:.3} < {MIN_PURITY}", g.purity),
                (RefusalReason::TooFewCells, Some(g)) => {
                    format!("{}x{} cells, fewer than {MIN_CELLS} on the short axis", g.cells[0], g.cells[1])
                }
                (RefusalReason::NoPeriod, _) => "no cell size found in the searched range".to_string(),
                (RefusalReason::InvalidSize, _) => "the cell size does not fit the image".to_string(),
                (r, None) => r.name().replace('_', " "),
            });
        }
        write!(f, "no pixel grid: {}", parts.join(", "))
    }
}

impl std::error::Error for Refusal {}

/// The cell grid of `img`, or a refusal when it doesn't look like pixel art.
pub fn detect(img: &RgbaImage, opts: &DetectOptions) -> Result<Grid, Refusal> {
    let (w, h) = img.dimensions();
    if w < 2 || h < 2 {
        return Err(Refusal::bare(if opts.size.is_some() { RefusalReason::InvalidSize } else { RefusalReason::NoPeriod }));
    }
    let axes = Axes::new(img);
    if let Some(size) = opts.size {
        let p = f64::from(size);
        if !p.is_finite() || p < 1.0 || p > f64::from(w.min(h)) {
            return Err(Refusal::bare(RefusalReason::InvalidSize));
        }
        return Ok(fixed_size(img, &axes, p));
    }

    // Step 2: candidate periods, square cells, so both axes' candidates are tried on both.
    let fx = estimate_axis(&axes.x.profile);
    let fy = estimate_axis(&axes.y.profile);
    let limit = f64::from(w.min(h)) / f64::from(MIN_CELLS);
    let min_cell = f64::from(opts.min_cell);
    let mut cands: Vec<f64> = [fx.map(|a| a.period), fy.map(|a| a.period), peak_lag(&axes.x.profile), peak_lag(&axes.y.profile)]
        .into_iter()
        .flatten()
        .filter(|&v| v >= min_cell && v <= limit)
        .map(|v| (v * 1e4).round() / 1e4)
        .collect();
    cands.sort_by(f64::total_cmp);
    cands.dedup();
    if cands.is_empty() {
        return Err(Refusal::bare(RefusalReason::NoPeriod));
    }

    // Steps 3-4: elastic fit per candidate, scored on the noise-floor-corrected capture.
    let mut fits: Vec<Fit> = cands.iter().map(|&p| axes.fit(p)).collect();
    let best = fits.iter().map(|f| f.capture).fold(f64::NEG_INFINITY, f64::max);
    let threshold = best - (1.0 - KEEP_SHARE) * best.abs();
    let kept: Vec<f64> = fits.iter().filter(|f| f.capture >= threshold).map(|f| f.period).collect();
    let top = kept.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let mut ks: Vec<f64> = kept.into_iter().filter(|&p| top / p < SUBMULTIPLE_RATIO).collect();
    ks.sort_by(f64::total_cmp);
    let mid = ks.len() / 2;
    let pm = if ks.len() % 2 == 1 { ks[mid] } else { 0.5 * (ks[mid - 1] + ks[mid]) };
    let mut pick = match fits.iter().position(|f| (f.period - pm).abs() <= 1e-6) {
        Some(i) => fits.swap_remove(i),
        None => axes.fit(pm),
    };
    // Refinement (not in the prototype): candidate periods run 1-3 % short on jittered grids, and
    // the DP then fights the period prior all along the axis. Refit once at the fit's own mean
    // inner cell width; keep it when it captures at least as much.
    let p2 = inner_period(&pick.bx, &pick.by);
    if (p2 - pick.period).abs() / pick.period > REFIT_SHARE {
        let refit = axes.fit(p2);
        if refit.capture >= pick.capture {
            pick = refit;
        }
    }

    // Step 5: a confident uniform FFT grid at the same period competes on purity.
    let regular = match (fx, fy) {
        (Some(ax), Some(ay))
            if ax.coherence.min(ay.coherence) >= REGULAR_COHERENCE
                && rel_diff(ax.period, ay.period) <= AXIS_AGREEMENT
                && (ax.period - pm).abs() / pm <= AXIS_AGREEMENT
                && (ay.period - pm).abs() / pm <= AXIS_AGREEMENT =>
        {
            Some((regular_bounds(ax.period, ax.offset, w), regular_bounds(ay.period, ay.offset, h)))
        }
        _ => None,
    };
    let grid = finish(img, &axes, pick, regular, cands);

    // Step 7: refusal.
    let mut reasons = Vec::new();
    if grid.capture < MIN_CAPTURE {
        reasons.push(RefusalReason::LowCapture);
    }
    if grid.purity < MIN_PURITY {
        reasons.push(RefusalReason::LowPurity);
    }
    if grid.cells[0].min(grid.cells[1]) < MIN_CELLS {
        reasons.push(RefusalReason::TooFewCells);
    }
    if reasons.is_empty() {
        Ok(grid)
    } else {
        Err(Refusal { reasons, grid: Some(Box::new(grid)) })
    }
}

/// One pixel per cell of `grid`: tolerant-mode colour of the cell's inner pixels, alpha 255 when
/// the cell's median alpha is at least 128, otherwise a transparent pixel.
pub fn snap(img: &RgbaImage, grid: &Grid) -> RgbaImage {
    let bx: Vec<f64> = grid.bx.iter().map(|&v| f64::from(v)).collect();
    let by: Vec<f64> = grid.by.iter().map(|&v| f64::from(v)).collect();
    cell_colours(img, &bx, &by).0
}

/// Step 8: a given cell size; the regular grid takes its phase from the edges and competes with
/// the elastic fit at the same size.
fn fixed_size(img: &RgbaImage, axes: &Axes, p: f64) -> Grid {
    let (w, h) = img.dimensions();
    let (ox, _) = phase_and_coherence(&axes.x.profile, p);
    let (oy, _) = phase_and_coherence(&axes.y.profile, p);
    let regular = (regular_bounds(p, ox, w), regular_bounds(p, oy, h));
    finish(img, axes, axes.fit(p), Some(regular), Vec::new())
}

/// Steps 5-6: keep the regular grid unless the elastic fit is purer, and measure the result.
fn finish(img: &RgbaImage, axes: &Axes, elastic: Fit, regular: Option<(Vec<f64>, Vec<f64>)>, cands: Vec<f64>) -> Grid {
    let elastic_bounds = (to_f32(&elastic.bx), to_f32(&elastic.by));
    let (_, elastic_purity) = cell_colours(img, &to_f64(&elastic_bounds.0), &to_f64(&elastic_bounds.1));
    let mut chosen = (Mode::Elastic, elastic_bounds, elastic_purity);
    if let Some((rx, ry)) = regular {
        let bounds = (to_f32(&rx), to_f32(&ry));
        let (_, purity) = cell_colours(img, &to_f64(&bounds.0), &to_f64(&bounds.1));
        if purity >= elastic_purity - ELASTIC_PURITY_MARGIN {
            chosen = (Mode::Regular, bounds, purity);
        }
    }
    let (mode, (bx, by), purity) = chosen;
    let (bx64, by64) = (to_f64(&bx), to_f64(&by));
    let capture = (capture(&axes.x.scoring, &bx64) + capture(&axes.y.scoring, &by64)) / 2.0;
    let px = mean_period(&bx64);
    let py = mean_period(&by64);
    Grid {
        period: [px as f32, py as f32],
        offset: [bx[0], by[0]],
        mode,
        cells: [(bx.len() - 1) as u32, (by.len() - 1) as u32],
        capture: capture as f32,
        purity: purity as f32,
        coherence: [
            phase_and_coherence(&axes.x.profile, px).1 as f32,
            phase_and_coherence(&axes.y.profile, py).1 as f32,
        ],
        candidates: cands.iter().map(|&v| v as f32).collect(),
        bx,
        by,
    }
}

/// An elastic fit at one period and its noise-floor-corrected capture.
struct Fit {
    period: f64,
    bx: Vec<f64>,
    by: Vec<f64>,
    capture: f64,
}

/// One axis's edge profile, edge strength (fitting) and noise-floor-corrected strength (scoring).
struct Axis {
    profile: Vec<f64>,
    edges: Vec<f64>,
    scoring: Vec<f64>,
    len: u32,
}

impl Axis {
    fn new(profile: Vec<f64>, len: u32) -> Self {
        let edges = edge_strength(&profile, len);
        let n = len as usize;
        // Step 4: subtract the median so a boundary on a flat stretch costs instead of harvesting
        // noise; the DP itself fits on the raw strengths.
        let floor = median(&edges[1..n]);
        let scoring = edges.iter().map(|v| v - floor).collect();
        Axis { profile, edges, scoring, len }
    }
}

struct Axes {
    x: Axis,
    y: Axis,
}

impl Axes {
    fn new(img: &RgbaImage) -> Self {
        let (w, h) = img.dimensions();
        let (px, py) = edge_profiles(img);
        Axes { x: Axis::new(px, w), y: Axis::new(py, h) }
    }

    fn fit(&self, period: f64) -> Fit {
        let bx = fit_boundaries(&self.x.edges, self.x.len, period);
        let by = fit_boundaries(&self.y.edges, self.y.len, period);
        let capture = (capture(&self.x.scoring, &bx) + capture(&self.y.scoring, &by)) / 2.0;
        Fit { period, bx, by, capture }
    }
}

/// Mean width of the inner cells of both axes (the outer cells may be cut by the image border).
fn inner_period(bx: &[f64], by: &[f64]) -> f64 {
    let span = |b: &[f64]| if b.len() > 3 { (b[b.len() - 2] - b[1], (b.len() - 3) as f64) } else { (b[b.len() - 1] - b[0], (b.len() - 1) as f64) };
    let ((wx, nx), (wy, ny)) = (span(bx), span(by));
    (wx + wy) / (nx + ny)
}

fn rel_diff(a: f64, b: f64) -> f64 {
    (a - b).abs() / a.max(b)
}

fn mean_period(bounds: &[f64]) -> f64 {
    (bounds[bounds.len() - 1] - bounds[0]) / (bounds.len() - 1) as f64
}

fn median(values: &[f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    let m = v.len() / 2;
    if v.len() % 2 == 1 { v[m] } else { 0.5 * (v[m - 1] + v[m]) }
}

fn to_f32(v: &[f64]) -> Vec<f32> {
    v.iter().map(|&x| x as f32).collect()
}

fn to_f64(v: &[f32]) -> Vec<f64> {
    v.iter().map(|&x| f64::from(x)).collect()
}

fn round4(v: f32) -> f64 {
    (f64::from(v) * 1e4).round() / 1e4
}

// ---- step 6: cell colour, alpha and purity ----

/// Inner pixel span `[lo, hi)` of each cell: `min(p/4, (p-1)/2)` trimmed from each side (unfake
/// `bound_spans`), at least the centre pixel.
fn spans(bounds: &[f64], n: u32) -> Vec<(u32, u32)> {
    let last = f64::from(n) - 1.0;
    bounds
        .windows(2)
        .map(|ab| {
            let (a, b) = (ab[0], ab[1]);
            let p = b - a;
            let margin = (0.25 * p).min(((p - 1.0) / 2.0).max(0.0));
            let centre = ((a + b) / 2.0).floor().clamp(0.0, last);
            let lo = (a + margin - 0.5).ceil().max(0.0);
            let hi = (b - margin - 0.5).floor().min(last);
            if hi >= lo { (lo as u32, hi as u32 + 1) } else { (centre as u32, centre as u32 + 1) }
        })
        .collect()
}

/// One pixel per cell and the mean purity of the opaque cells.
fn cell_colours(img: &RgbaImage, bx: &[f64], by: &[f64]) -> (RgbaImage, f64) {
    let xs = spans(bx, img.width());
    let ys = spans(by, img.height());
    let mut out = RgbaImage::new(xs.len() as u32, ys.len() as u32);
    let (mut purity_sum, mut opaque) = (0.0, 0usize);
    let mut alphas = Vec::new();
    let mut px: Vec<[i32; 3]> = Vec::new();
    let mut support = Vec::new();
    for (j, &(y0, y1)) in ys.iter().enumerate() {
        for (i, &(x0, x1)) in xs.iter().enumerate() {
            alphas.clear();
            px.clear();
            for y in y0..y1 {
                for x in x0..x1 {
                    let p = img.get_pixel(x, y).0;
                    alphas.push(f64::from(p[3]));
                    if p[3] >= 128 {
                        px.push([i32::from(p[0]), i32::from(p[1]), i32::from(p[2])]);
                    }
                }
            }
            if median(&alphas) < 128.0 || px.is_empty() {
                continue;
            }
            let (colour, purity) = tolerant_mode(&px, &mut support);
            purity_sum += purity;
            opaque += 1;
            out.put_pixel(i as u32, j as u32, Rgba([colour[0], colour[1], colour[2], 255]));
        }
    }
    (out, if opaque > 0 { purity_sum / opaque as f64 } else { 0.0 })
}

fn close(a: &[i32; 3], b: &[i32; 3]) -> bool {
    let d: i32 = (0..3).map(|c| (a[c] - b[c]).pow(2)).sum();
    d <= COLOUR_TOLERANCE * COLOUR_TOLERANCE
}

/// The pixel with the most neighbours within the tolerance; the colour is the mean of those
/// neighbours, the purity the share of all pixels within the tolerance of that colour. Above
/// `MODE_SAMPLE` pixels (inner spans of cells over ~32 px) the most-supported pixel is searched
/// among an even sample of them, so the cost per cell stays bounded; colour and purity still use
/// every pixel.
fn tolerant_mode(px: &[[i32; 3]], support: &mut Vec<u32>) -> ([u8; 3], f64) {
    let stride = px.len().div_ceil(MODE_SAMPLE);
    let sample: Vec<&[i32; 3]> = px.iter().step_by(stride).collect();
    support.clear();
    support.resize(sample.len(), 1);
    for a in 0..sample.len() {
        for b in a + 1..sample.len() {
            if close(sample[a], sample[b]) {
                support[a] += 1;
                support[b] += 1;
            }
        }
    }
    let mut k = 0;
    for (i, &s) in support.iter().enumerate() {
        if s > support[k] {
            k = i;
        }
    }
    let centre = *sample[k];
    let mut sum = [0.0f64; 3];
    let mut count = 0.0;
    for p in px.iter().filter(|p| close(p, &centre)) {
        for c in 0..3 {
            sum[c] += f64::from(p[c]);
        }
        count += 1.0;
    }
    let mean = sum.map(|s| s / count);
    let colour = mean.map(|v| v.round().clamp(0.0, 255.0) as u8);
    let tol2 = f64::from(COLOUR_TOLERANCE * COLOUR_TOLERANCE);
    let within = px
        .iter()
        .filter(|p| (0..3).map(|c| (f64::from(p[c]) - mean[c]).powi(2)).sum::<f64>() <= tol2)
        .count();
    (colour, within as f64 / px.len() as f64)
}

// ---- unfake detect/grid.rs ----

/// Column / row edge profiles; entry `i` is the boundary at coordinate `i + 1`.
/// Premultiplied RGB + alpha so transparent pixels' colour doesn't add noise.
fn edge_profiles(img: &RgbaImage) -> (Vec<f64>, Vec<f64>) {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let raw = img.as_raw();
    let mut prof_x = vec![0.0f64; w.saturating_sub(1)];
    let mut prof_y = vec![0.0f64; h.saturating_sub(1)];
    let mut prev: Vec<[i32; 4]> = Vec::with_capacity(w);
    let mut cur: Vec<[i32; 4]> = Vec::with_capacity(w);
    for y in 0..h {
        cur.clear();
        cur.extend(raw[y * w * 4..(y + 1) * w * 4].as_chunks::<4>().0.iter().map(premul));
        for x in 0..w.saturating_sub(1) {
            prof_x[x] += f64::from(l1(&cur[x + 1], &cur[x]));
        }
        if y > 0 {
            let s: i64 = cur.iter().zip(&prev).map(|(a, b)| i64::from(l1(a, b))).sum();
            prof_y[y - 1] = s as f64;
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    (prof_x, prof_y)
}

fn premul(p: &[u8; 4]) -> [i32; 4] {
    let a = i32::from(p[3]);
    [i32::from(p[0]) * a / 255, i32::from(p[1]) * a / 255, i32::from(p[2]) * a / 255, a]
}

fn l1(a: &[i32; 4], b: &[i32; 4]) -> i32 {
    (a[0] - b[0]).abs() + (a[1] - b[1]).abs() + (a[2] - b[2]).abs() + (a[3] - b[3]).abs()
}

/// Shortest period searched by the FFT.
const PMIN: f64 = 2.1;
/// Harmonic counts as present at this share of the strongest peak.
const HARMONIC_RATIO: f64 = 0.3;
/// Peaks at this share of the strongest are candidates for the fundamental.
const NEAR_PEAK: f64 = 0.8;
/// A lower candidate replaces the peak only if it keeps this share of coherence.
const SUBHARMONIC_COHERENCE: f64 = 0.75;

#[derive(Debug, Clone, Copy, PartialEq)]
struct AxisGrid {
    period: f64,
    /// A boundary lies at `offset + i · period`, `offset` in `[0, period)`.
    offset: f64,
    coherence: f64,
}

/// Period + phase of the boundary comb along one axis (unfake `estimate_axis`; the prominence it
/// also returns is not used here).
fn estimate_axis(profile: &[f64]) -> Option<AxisGrid> {
    let m = profile.len();
    if m < 16 {
        return None;
    }
    let n = (m + 1) as f64;
    let pmax = (n / 6.0).clamp(4.0, 160.0);
    let mean = profile.iter().sum::<f64>() / m as f64;
    let windowed: Vec<f64> = profile
        .iter()
        .enumerate()
        .map(|(i, v)| (v - mean) * (0.5 - 0.5 * (2.0 * PI * i as f64 / (m - 1) as f64).cos()))
        .collect();
    let spec = Spectrum::new(&windowed);

    let (fmin, fmax) = (1.0 / pmax, 1.0 / PMIN);
    let (k0, k1) = (spec.bin_ceil(fmin), spec.bin_floor(fmax));
    if k1 <= k0 {
        return None;
    }
    let band = &spec.mag[k0..=k1];
    let (peak_i, &amax) = band.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1))?;
    if amax <= 0.0 {
        return None;
    }

    // Near-equal peaks with equal coherence are one comb seen twice; the lowest is the cell size.
    let argmax_f = (k0 + peak_i) as f64 / spec.n_fft as f64;
    let coh_max = coherence_at(profile, argmax_f);
    let last = spec.mag.len() - 1;
    let fs = (k0..=k1)
        .filter(|&k| {
            let v = spec.mag[k];
            v >= NEAR_PEAK * amax && v >= spec.mag[k - 1] && v >= spec.mag[(k + 1).min(last)]
        })
        .map(|k| k as f64 / spec.n_fft as f64)
        .find(|&f| coherence_at(profile, f) >= SUBHARMONIC_COHERENCE * coh_max)
        .unwrap_or(argmax_f);

    // Largest divisor d with every harmonic k·fs/d (k < d) present that keeps the edges on its lines.
    let floor = HARMONIC_RATIO * amax;
    let coh_fs = coherence_at(profile, fs);
    let mut best_d = 1usize;
    for d in 2..=(fs / fmin).floor() as usize {
        let f0 = fs / d as f64;
        if (1..d).all(|k| spec.local(k as f64 * f0) >= floor) && coherence_at(profile, f0) >= SUBHARMONIC_COHERENCE * coh_fs {
            best_d = d;
        }
    }
    let f0 = fs / best_d as f64;

    let harmonics: Vec<f64> = (1..=8).map(f64::from).filter(|&k| k * f0 < 0.5 && spec.local(k * f0) >= floor).collect();
    let score = |fr: f64| -> f64 { harmonics.iter().map(|&k| dft_mag(&windowed, k * fr)).sum() };

    let (mut lo, mut hi) = (f0 - 1.5 / m as f64, f0 + 1.5 / m as f64);
    let mut best_f = f0;
    for _ in 0..6 {
        let step = (hi - lo) / 32.0;
        let mut best_v = f64::MIN;
        for i in 0..=32 {
            let f = lo + step * f64::from(i);
            let v = score(f);
            if v > best_v {
                best_v = v;
                best_f = f;
            }
        }
        lo = best_f - 2.0 * step;
        hi = best_f + 2.0 * step;
    }

    let mut period = 1.0 / best_f;
    // Resampled from a whole N-cell canvas: lock to exactly n / N.
    let cells = n / period;
    let whole = cells.round();
    if whole >= 1.0 && (cells - whole).abs() < 0.25 && score(whole / n) >= 0.97 * score(best_f) {
        period = n / whole;
    }
    let (offset, coherence) = phase_and_coherence(profile, period);
    Some(AxisGrid { period, offset, coherence })
}

fn coherence_at(profile: &[f64], f: f64) -> f64 {
    phase_and_coherence(profile, 1.0 / f).1
}

/// Boundary phase in `[0, period)` and the aligned share of edge energy.
fn phase_and_coherence(profile: &[f64], period: f64) -> (f64, f64) {
    let total: f64 = profile.iter().sum();
    if total <= 0.0 {
        return (0.0, 0.0);
    }
    let (re, im) = dft(profile, 1.0 / period);
    let coherence = (re * re + im * im).sqrt() / total;
    let offset = (-im.atan2(re) * period / (2.0 * PI)).rem_euclid(period);
    (offset, coherence)
}

/// Boundaries of the cells of a regular grid whose centre lies inside `[0, len)` (unfake
/// `AxisGrid::bounds`); the outer two may lie outside the image.
fn regular_bounds(period: f64, offset: f64, len: u32) -> Vec<f64> {
    let first = (-offset / period - 0.5).ceil();
    let last = ((f64::from(len) - offset) / period - 0.5).ceil() - 1.0;
    let cells = (last - first + 1.0).max(1.0) as usize;
    (0..=cells).map(|i| offset + (first + i as f64) * period).collect()
}

/// Σ sig[i]·e^{-2πi·f·(i+1)} via a rotating phasor.
fn dft(sig: &[f64], f: f64) -> (f64, f64) {
    let w = -2.0 * PI * f;
    let (sw, cw) = w.sin_cos();
    let (mut zr, mut zi) = (cw, sw);
    let (mut re, mut im) = (0.0, 0.0);
    for (i, &v) in sig.iter().enumerate() {
        re += v * zr;
        im += v * zi;
        let nr = zr * cw - zi * sw;
        zi = zr * sw + zi * cw;
        zr = nr;
        if i % 1024 == 1023 {
            // Keep the phasor on the unit circle over long profiles.
            let norm = (zr * zr + zi * zi).sqrt();
            zr /= norm;
            zi /= norm;
        }
    }
    (re, im)
}

fn dft_mag(sig: &[f64], f: f64) -> f64 {
    let (re, im) = dft(sig, f);
    (re * re + im * im).sqrt()
}

/// Magnitude spectrum, zero-padded ×8 for peak resolution.
struct Spectrum {
    mag: Vec<f64>,
    n_fft: usize,
    /// Unpadded length: sets the main-lobe width.
    m: usize,
}

impl Spectrum {
    fn new(sig: &[f64]) -> Self {
        let m = sig.len();
        let n_fft = (m * 8).next_power_of_two();
        let mut re = vec![0.0; n_fft];
        let mut im = vec![0.0; n_fft];
        re[..m].copy_from_slice(sig);
        fft(&mut re, &mut im);
        let mag = (0..=n_fft / 2).map(|k| (re[k] * re[k] + im[k] * im[k]).sqrt()).collect();
        Self { mag, n_fft, m }
    }

    fn bin_ceil(&self, f: f64) -> usize {
        ((f * self.n_fft as f64).ceil() as usize).min(self.mag.len() - 1)
    }

    fn bin_floor(&self, f: f64) -> usize {
        ((f * self.n_fft as f64).floor() as usize).min(self.mag.len() - 1)
    }

    /// Peak magnitude within the main lobe around `f`.
    fn local(&self, f: f64) -> f64 {
        let half = 1.5 / self.m as f64;
        let lo = self.bin_ceil((f - half).max(0.0));
        let hi = self.bin_floor(f + half);
        if hi < lo {
            return 0.0;
        }
        self.mag[lo..=hi].iter().copied().fold(0.0, f64::max)
    }
}

/// In-place iterative radix-2 FFT (`re.len()` must be a power of two).
fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    let mut j = 0usize;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let (sw, cw) = (-2.0 * PI / len as f64).sin_cos();
        for start in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0f64, 0.0f64);
            for k in 0..len / 2 {
                let a = start + k;
                let b = a + len / 2;
                let tr = re[b] * cr - im[b] * ci;
                let ti = re[b] * ci + im[b] * cr;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                let nr = cr * cw - ci * sw;
                ci = cr * sw + ci * cw;
                cr = nr;
            }
        }
        len <<= 1;
    }
}

// ---- unfake detect/edge.rs ----

/// First autocorrelation peak at ≥ 0.6 × the maximum, lags 2..=min(n/8, 128) (unfake `peak_lag`).
/// Changed: refined to sub-pixel by a parabola through the peak, and `None` without a positive peak.
fn peak_lag(sig: &[f64]) -> Option<f64> {
    let n = sig.len();
    if n < 4 {
        return None;
    }
    let max_lag = (n / 8).clamp(2, 128);
    let mean = sig.iter().sum::<f64>() / n as f64;
    let s: Vec<f64> = sig.iter().map(|v| v - mean).collect();
    let norm = s.iter().map(|v| v * v).sum::<f64>().max(1e-9);
    let mut ac = vec![0.0; max_lag + 2];
    for lag in 2..=max_lag.min(n - 1) {
        ac[lag] = s[..n - lag].iter().zip(&s[lag..]).map(|(a, b)| a * b).sum::<f64>() / norm;
    }
    let mut start = 2usize;
    let cap = (max_lag / 2).max(3);
    while start < cap && ac[start + 1] <= ac[start] {
        start += 1;
    }
    let (mut gmax, mut gp) = (f64::NEG_INFINITY, start);
    for (p, &v) in ac.iter().enumerate().take(max_lag + 1).skip(start) {
        if v > gmax {
            gmax = v;
            gp = p;
        }
    }
    if gmax <= 0.0 {
        return None;
    }
    let thr = 0.6 * gmax;
    for p in start..=max_lag {
        if ac[p] >= thr && ac[p] >= ac[p - 1] && ac[p] >= ac[p + 1] {
            let (a, b, c) = (ac[p - 1], ac[p], ac[p + 1]);
            let den = a - 2.0 * b + c;
            return Some(p as f64 + if den < 0.0 { 0.5 * (a - c) / den } else { 0.0 });
        }
    }
    Some(gp as f64)
}

// ---- unfake detect/elastic.rs ----

/// Boundary positions per source pixel (¼ px).
const LATTICE: usize = 4;
const MIN_SPACING: f64 = 0.75;
const MAX_SPACING: f64 = 1.3;
/// A cell off the period by this share costs as much as a strong edge earns.
const SLACK: f64 = 0.25;

/// Edge strength at integer boundary coordinates `0..=len` (entry `c` is the boundary between
/// pixels `c - 1` and `c`), with half credit one pixel off for edges spread over two pixels,
/// normalised to its 90th percentile.
fn edge_strength(profile: &[f64], len: u32) -> Vec<f64> {
    let n = len as usize;
    let mut raw = vec![0.0; n + 1];
    raw[1..n].copy_from_slice(&profile[..n - 1]);
    let mut e: Vec<f64> = (0..=n)
        .map(|c| {
            let left = if c > 0 { raw[c - 1] } else { 0.0 };
            let right = if c < n { raw[c + 1] } else { 0.0 };
            raw[c].max(0.5 * left.max(right))
        })
        .collect();
    let mut inner: Vec<f64> = e[1..n].to_vec();
    inner.sort_by(f64::total_cmp);
    let p90 = inner.get(inner.len() * 9 / 10).copied().unwrap_or(0.0).max(1e-9);
    e.iter_mut().for_each(|v| *v /= p90);
    e
}

/// Share of the edge energy that lies on the boundaries. Changed: the total counts only positive
/// strengths, so with noise-floor-corrected strengths a boundary on a flat stretch costs.
fn capture(edges: &[f64], bounds: &[f64]) -> f64 {
    let n = edges.len() - 1;
    let total: f64 = edges[1..n].iter().map(|v| v.max(0.0)).sum();
    let hit: f64 = bounds
        .iter()
        .map(|b| b.round())
        .filter(|&c| c > 0.0 && (c as usize) < n)
        .map(|c| edges[c as usize])
        .sum();
    hit / total.max(1e-9)
}

/// DP over the ¼-px lattice: boundaries (the first ≤ 0, the last ≥ len) that maximise edge
/// strength on them minus a quadratic cost per cell for leaving the period. Returns the boundaries
/// of cells whose centre is inside the image.
fn fit_boundaries(edges: &[f64], len: u32, period: f64) -> Vec<f64> {
    let q = LATTICE as f64;
    let n = len as usize;
    let steps = n * LATTICE;
    let smin = ((MIN_SPACING * period * q).floor() as usize).max(1);
    let smax = ((MAX_SPACING * period * q).ceil() as usize).max(smin + 1);
    let lambda = 1.0 / (SLACK * period).powi(2);
    let penalty: Vec<f64> = (smin..=smax).map(|s| lambda * (s as f64 / q - period).powi(2)).collect();
    // Edge strength on the lattice (linear between pixel boundaries).
    let gain = |t: usize| -> f64 {
        if t == 0 || t >= steps {
            return 0.0;
        }
        let (c, f) = (t / LATTICE, (t % LATTICE) as f64 / q);
        edges[c] * (1.0 - f) + edges[(c + 1).min(n)] * f
    };

    // Index j = lattice position + smax, positions from -smax to steps + smax.
    let size = steps + 2 * smax + 1;
    let mut best = vec![f64::NEG_INFINITY; size];
    let mut prev = vec![usize::MAX; size];
    for b in best.iter_mut().take(smax + 1).skip(1) {
        *b = 0.0; // the first boundary: anywhere in (-smax, 0]
    }
    for j in smax + 1..size {
        let mut top = f64::NEG_INFINITY;
        let mut arg = usize::MAX;
        for (k, s) in (smin..=smax).enumerate() {
            if s > j {
                break;
            }
            let v = best[j - s] - penalty[k];
            if v > top {
                top = v;
                arg = j - s;
            }
        }
        if arg != usize::MAX {
            best[j] = top + gain(j - smax);
            prev[j] = arg;
        }
    }
    let mut j = (steps + smax..size).max_by(|&a, &b| best[a].total_cmp(&best[b])).unwrap_or(size - 1);
    let mut bounds = vec![(j as f64 - smax as f64) / q];
    while prev[j] != usize::MAX {
        j = prev[j];
        bounds.push((j as f64 - smax as f64) / q);
    }
    bounds.reverse();

    // Keep the run of cells whose centre lies inside [0, len).
    let inside = |i: usize| {
        let c = (bounds[i] + bounds[i + 1]) / 2.0;
        c >= 0.0 && c < f64::from(len)
    };
    let first = (0..bounds.len() - 1).find(|&i| inside(i));
    let last = (0..bounds.len() - 1).rev().find(|&i| inside(i));
    match (first, last) {
        (Some(a), Some(b)) => bounds[a..=b + 1].to_vec(),
        _ => bounds,
    }
}

#[cfg(test)]
mod fixtures {
    //! Port of the research prototype's `fixtures.py`: procedural pico-8 art rendered onto known
    //! (fractional, jittered or drifting) cell boundaries with a 4×4 box filter, then Gaussian blur,
    //! additive noise and a JPEG round-trip, optionally with transparency. Deterministic xorshift.
    use image::{Rgb, RgbImage, Rgba, RgbaImage};

    pub const PICO8: [[u8; 3]; 16] = [
        [0, 0, 0], [29, 43, 83], [126, 37, 83], [0, 135, 81], [171, 82, 54], [95, 87, 79],
        [194, 195, 199], [255, 241, 232], [255, 0, 77], [255, 163, 0], [255, 236, 39],
        [0, 228, 54], [41, 173, 255], [131, 118, 156], [255, 119, 168], [255, 204, 170],
    ];

    /// xorshift64* seeded through splitmix64.
    pub struct Rng(u64);

    impl Rng {
        pub fn new(seed: u64) -> Self {
            let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            Rng((z ^ (z >> 31)) | 1)
        }

        pub fn next_u64(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            self.0 = x;
            x.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }

        /// Uniform in `[0, 1)`.
        pub fn f64(&mut self) -> f64 {
            (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
        }

        pub fn uniform(&mut self, lo: f64, hi: f64) -> f64 {
            lo + (hi - lo) * self.f64()
        }

        /// Integer in `[lo, hi)` (numpy `integers`).
        pub fn int(&mut self, lo: i64, hi: i64) -> i64 {
            assert!(hi > lo, "empty range {lo}..{hi}");
            lo + (self.f64() * (hi - lo) as f64) as i64
        }

        pub fn normal(&mut self) -> f64 {
            let u1 = self.f64().max(1e-12);
            let u2 = self.f64();
            (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq)]
    pub enum GridMode {
        Uniform,
        /// Every cell width independently ±20 % (hand-drawn).
        Jitter,
        /// Cell size drifts linearly from 0.93 to 1.07 × the period.
        Drift,
    }

    #[derive(Debug, Clone)]
    pub struct Spec {
        pub name: String,
        pub cells: (usize, usize),
        pub period: f64,
        pub offset: (f64, f64),
        pub mode: GridMode,
        pub transparent: bool,
        pub blur: f64,
        pub noise: f64,
        pub jpeg: u8,
    }

    /// A `fixtures.py` row: name, cells, period, offset, grid mode, transparent, blur, noise, jpeg.
    type Row<'a> = (&'a str, (usize, usize), f64, (f64, f64), GridMode, bool, f64, f64, u8);

    fn spec((name, cells, period, offset, mode, transparent, blur, noise, jpeg): Row) -> Spec {
        Spec { name: name.to_string(), cells, period, offset, mode, transparent, blur, noise, jpeg }
    }

    /// The fixed cases A–G of `fixtures.py`.
    pub fn fixed() -> Vec<Spec> {
        use GridMode::*;
        vec![
            spec(("A_uniform_7.3", (128, 96), 7.3, (3.4, 1.7), Uniform, false, 0.8, 4.0, 80)),
            spec(("B_jitter_8.4", (112, 84), 8.4, (5.0, 2.2), Jitter, false, 0.8, 4.0, 80)),
            spec(("C_drift_7.4", (128, 96), 7.4, (1.2, 4.6), Drift, false, 0.8, 4.0, 80)),
            spec(("D_alpha_9.6", (96, 96), 9.6, (40.3, 22.8), Uniform, true, 1.0, 4.0, 85)),
            spec(("E_heavy_6.2", (144, 112), 6.2, (2.9, 0.4), Uniform, false, 1.2, 8.0, 60)),
            spec(("F_integer_8", (120, 90), 8.0, (3.0, 5.0), Uniform, false, 0.8, 4.0, 80)),
            spec(("G_jitter_alpha_7.7", (100, 120), 7.7, (30.0, 12.5), Jitter, true, 0.9, 5.0, 80)),
        ]
    }

    fn round_to(v: f64, digits: i32) -> f64 {
        let s = 10f64.powi(digits);
        (v * s).round() / s
    }

    /// `fixtures.py::holdout`: period 5.5–11, any grid mode, random degradation.
    pub fn holdout(n: usize, seed: u64) -> Vec<Spec> {
        let mut r = Rng::new(seed);
        (0..n)
            .map(|k| {
                let p = round_to(r.uniform(5.5, 11.0), 2);
                let (cw, ch) = (r.int(64, 150) as usize, r.int(64, 150) as usize);
                let cap = (1100.0 / p) as usize;
                let mode = [GridMode::Uniform, GridMode::Uniform, GridMode::Jitter, GridMode::Drift][k % 4];
                let tr = r.int(0, 2) == 1;
                let span = if tr { 60.0 } else { p };
                let off = (round_to(r.uniform(0.0, span), 1), round_to(r.uniform(0.0, span), 1));
                let blur = round_to(r.uniform(0.5, 1.3), 2);
                let noise = round_to(r.uniform(2.0, 9.0), 1);
                let jpeg = r.int(55, 92) as u8;
                let name = format!("H{k:02}_{mode:?}_{p}{}", if tr { "_a" } else { "" });
                spec((&name, (cw.min(cap), ch.min(cap)), p, off, mode, tr, blur, noise, jpeg))
            })
            .collect()
    }

    /// Palette-index art, `ch` rows of `cw`; -1 = transparent.
    pub fn make_art(cw: usize, ch: usize, rng: &mut Rng, transparent: bool) -> Vec<i16> {
        let bg = if transparent { -1 } else { 1 };
        let mut a = vec![bg; cw * ch];
        let (cwi, chi) = (cw as i64, ch as i64);
        let n_shapes = 6.max(cw * ch / 300);
        for _ in 0..n_shapes {
            let col = rng.int(2, 16) as i16;
            let kind = rng.int(0, 3);
            let (cx, cy) = (rng.int(0, cwi), rng.int(0, chi));
            let (rx, ry) = (rng.int(3, 4.max(cwi / 6)), rng.int(3, 4.max(chi / 6)));
            let m: Vec<bool> = (0..ch * cw)
                .map(|i| {
                    let (x, y) = ((i % cw) as i64, (i / cw) as i64);
                    match kind {
                        0 => {
                            let (dx, dy) = ((x - cx) as f64 / rx as f64, (y - cy) as f64 / ry as f64);
                            dx * dx + dy * dy <= 1.0
                        }
                        1 => (x - cx).abs() <= rx && (y - cy).abs() <= ry,
                        _ => (x - cx).abs() + (y - cy).abs() <= rx,
                    }
                })
                .collect();
            let hl_col = if col != 7 && col != 15 { 7 } else { 6 };
            for i in 0..ch * cw {
                if m[i] {
                    let (x, y) = ((i % cw) as i64, (i / cw) as i64);
                    a[i] = if ((x - cx) + (y - cy)) as f64 > -(rx as f64) * 0.6 { col } else { hl_col };
                }
            }
            // dark 1-cell outline (no outline against the image border, as in numpy)
            for y in 0..ch {
                for x in 0..cw {
                    let i = y * cw + x;
                    if !m[i] {
                        continue;
                    }
                    let inner = (y == 0 || m[i - cw]) && (y + 1 == ch || m[i + cw]) && (x == 0 || m[i - 1]) && (x + 1 == cw || m[i + 1]);
                    if !inner {
                        a[i] = 0;
                    }
                }
            }
        }
        // dither patches (1-cell checkerboard)
        for _ in 0..3 {
            let (x0, y0) = (rng.int(0, cwi - 8) as usize, rng.int(0, chi - 8) as usize);
            let (w, h) = (rng.int(4, 10) as usize, rng.int(4, 10) as usize);
            let (c1, c2) = (rng.int(2, 16) as i16, rng.int(2, 16) as i16);
            for y in y0..(y0 + h).min(ch) {
                for x in x0..(x0 + w).min(cw) {
                    a[y * cw + x] = if (x + y) % 2 == 0 { c1 } else { c2 };
                }
            }
        }
        // isolated single-cell details and 1-cell lines
        for _ in 0..cw * ch / 120 {
            let y = rng.int(0, chi) as usize;
            let x = rng.int(0, cwi) as usize;
            a[y * cw + x] = rng.int(2, 16) as i16;
        }
        for _ in 0..4 {
            let y = rng.int(0, chi) as usize;
            let x0 = rng.int(0, cwi - 10) as usize;
            let len = rng.int(5, 15) as usize;
            let c = rng.int(2, 16) as i16;
            for x in x0..(x0 + len).min(cw) {
                a[y * cw + x] = c;
            }
        }
        a
    }

    /// `n + 1` increasing boundaries in image px; cell `i` spans `[b[i], b[i + 1])`.
    pub fn boundaries(n: usize, period: f64, offset: f64, mode: GridMode, rng: &mut Rng) -> Vec<f64> {
        let widths: Vec<f64> = match mode {
            GridMode::Uniform => vec![period; n],
            GridMode::Jitter => (0..n).map(|_| period * (1.0 + rng.uniform(-0.2, 0.2))).collect(),
            GridMode::Drift => (0..n).map(|i| period * (0.93 + 0.14 * i as f64 / (n - 1).max(1) as f64)).collect(),
        };
        let mut b = Vec::with_capacity(n + 1);
        b.push(offset);
        let mut acc = offset;
        for w in widths {
            acc += w;
            b.push(acc);
        }
        b
    }

    /// Straight RGBA floats, `w × h`.
    pub struct Float {
        pub w: usize,
        pub h: usize,
        pub px: Vec<[f64; 4]>,
    }

    /// Box-filtered render: each pixel is the premultiplied mean over 4 × 4 subsamples; the margin
    /// right/bottom equals the offset left/top, outside the art is background.
    pub fn render(art: &[i16], cw: usize, bx: &[f64], by: &[f64]) -> Float {
        const SS: usize = 4;
        let ch = art.len() / cw;
        let w = (bx[bx.len() - 1] + bx[0]).ceil() as usize;
        let h = (by[by.len() - 1] + by[0]).ceil() as usize;
        let bgv: i16 = if art.contains(&-1) { -1 } else { 1 };
        let index = |bounds: &[f64], count: usize, s: f64| -> Option<usize> {
            let k = bounds.partition_point(|&b| b <= s); // searchsorted(side="right")
            (k >= 1 && k - 1 < count).then(|| k - 1)
        };
        let ix: Vec<Option<usize>> = (0..w * SS).map(|i| index(bx, cw, (i as f64 + 0.5) / SS as f64)).collect();
        let iy: Vec<Option<usize>> = (0..h * SS).map(|i| index(by, ch, (i as f64 + 0.5) / SS as f64)).collect();
        let mut px = vec![[0.0; 4]; w * h];
        for y in 0..h {
            for x in 0..w {
                let mut acc = [0.0f64; 4];
                for sy in 0..SS {
                    for sx in 0..SS {
                        let v = match (ix[x * SS + sx], iy[y * SS + sy]) {
                            (Some(i), Some(j)) => art[j * cw + i],
                            _ => bgv,
                        };
                        if v >= 0 {
                            let c = PICO8[v as usize];
                            acc[0] += f64::from(c[0]);
                            acc[1] += f64::from(c[1]);
                            acc[2] += f64::from(c[2]);
                            acc[3] += 255.0;
                        }
                    }
                }
                let n = (SS * SS) as f64;
                let a = acc[3] / n;
                px[y * w + x] = if a > 0.0 {
                    [acc[0] / n * 255.0 / a, acc[1] / n * 255.0 / a, acc[2] / n * 255.0 / a, a]
                } else {
                    [0.0, 0.0, 0.0, 0.0]
                };
            }
        }
        Float { w, h, px }
    }

    /// Separable Gaussian blur of one channel plane (edge pixels repeated).
    fn blur_plane(plane: &[f64], w: usize, h: usize, sigma: f64) -> Vec<f64> {
        if sigma <= 0.0 {
            return plane.to_vec();
        }
        let r = (3.0 * sigma).ceil() as i64;
        let k: Vec<f64> = (-r..=r).map(|i| (-(i * i) as f64 / (2.0 * sigma * sigma)).exp()).collect();
        let sum: f64 = k.iter().sum();
        let k: Vec<f64> = k.iter().map(|v| v / sum).collect();
        let mut tmp = vec![0.0; w * h];
        for y in 0..h {
            for x in 0..w {
                tmp[y * w + x] = (-r..=r).map(|i| k[(i + r) as usize] * plane[y * w + (x as i64 + i).clamp(0, w as i64 - 1) as usize]).sum();
            }
        }
        let mut out = vec![0.0; w * h];
        for y in 0..h {
            for x in 0..w {
                out[y * w + x] = (-r..=r).map(|i| k[(i + r) as usize] * tmp[(y as i64 + i).clamp(0, h as i64 - 1) as usize * w + x]).sum();
            }
        }
        out
    }

    /// Blur RGB and alpha, add noise to RGB, JPEG round-trip of RGB (quality 0 skips it).
    pub fn degrade(img: &Float, blur: f64, noise: f64, jpeg: u8, rng: &mut Rng) -> RgbaImage {
        let (w, h) = (img.w, img.h);
        let quant = |v: f64| v.clamp(0.0, 255.0).floor();
        let planes: Vec<Vec<f64>> = (0..4)
            .map(|c| {
                let plane: Vec<f64> = img.px.iter().map(|p| quant(p[c])).collect();
                blur_plane(&plane, w, h, blur).into_iter().map(|v| v.round()).collect()
            })
            .collect();
        let mut rgb = RgbImage::new(w as u32, h as u32);
        for (i, p) in rgb.pixels_mut().enumerate() {
            let mut c = [0u8; 3];
            for (ch, v) in c.iter_mut().enumerate() {
                *v = quant(planes[ch][i] + noise * rng.normal()) as u8;
            }
            *p = Rgb(c);
        }
        if jpeg > 0 {
            let mut buf = Vec::new();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, jpeg).encode_image(&rgb).unwrap();
            rgb = image::load_from_memory(&buf).unwrap().to_rgb8();
        }
        RgbaImage::from_fn(w as u32, h as u32, |x, y| {
            let c = rgb.get_pixel(x, y).0;
            Rgba([c[0], c[1], c[2], planes[3][y as usize * w + x as usize] as u8])
        })
    }

    pub struct Fixture {
        pub spec: Spec,
        pub image: RgbaImage,
        /// Ground truth, one pixel per cell.
        pub truth: RgbaImage,
    }

    pub fn truth_image(art: &[i16], cw: usize) -> RgbaImage {
        RgbaImage::from_fn(cw as u32, (art.len() / cw) as u32, |x, y| {
            let v = art[y as usize * cw + x as usize];
            if v >= 0 {
                let c = PICO8[v as usize];
                Rgba([c[0], c[1], c[2], 255])
            } else {
                Rgba([0, 0, 0, 0])
            }
        })
    }

    pub fn generate(spec: &Spec, seed: u64) -> Fixture {
        let mut rng = Rng::new(seed);
        let (cw, ch) = spec.cells;
        let art = make_art(cw, ch, &mut rng, spec.transparent);
        let bx = boundaries(cw, spec.period, spec.offset.0, spec.mode, &mut rng);
        let by = boundaries(ch, spec.period, spec.offset.1, spec.mode, &mut rng);
        let image = degrade(&render(&art, cw, &bx, &by), spec.blur, spec.noise, spec.jpeg, &mut rng);
        Fixture { spec: spec.clone(), image, truth: truth_image(&art, cw) }
    }

    fn nearest(c: &[u8]) -> (usize, f64) {
        PICO8
            .iter()
            .enumerate()
            .map(|(i, p)| (i, (0..3).map(|k| (f64::from(c[k]) - f64::from(p[k])).powi(2)).sum::<f64>().sqrt()))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .unwrap()
    }

    /// `eval_synth.py::score`: the best integer shift of the truth inside `out`, then the share of
    /// truth cells whose alpha agrees and whose colour maps to the same pico-8 index within 48.
    pub fn score(out: &RgbaImage, truth: &RgbaImage, period: f64, offset: (f64, f64)) -> f64 {
        let (cw, ch) = (truth.width() as i64, truth.height() as i64);
        let (ow, oh) = (out.width() as i64, out.height() as i64);
        // per out cell: opaque, pico index (or None when farther than 48)
        let o: Vec<(bool, Option<usize>)> = out
            .pixels()
            .map(|p| {
                let (i, d) = nearest(&p.0);
                (p.0[3] >= 128, (d <= 48.0).then_some(i))
            })
            .collect();
        let g: Vec<(bool, usize)> = truth.pixels().map(|p| (p.0[3] >= 128, nearest(&p.0).0)).collect();
        let mx = (offset.0 / period).ceil() as i64 + 3;
        let my = (offset.1 / period).ceil() as i64 + 3;
        let mut best = 0usize;
        for dy in -3..=my {
            for dx in -3..=mx {
                let (j0, i0) = (0.max(-dy), 0.max(-dx));
                let (j1, i1) = (ch.min(oh - dy), cw.min(ow - dx));
                if j1 <= j0 || i1 <= i0 {
                    continue;
                }
                let mut ok = 0usize;
                for j in j0..j1 {
                    for i in i0..i1 {
                        let (ga, gi) = g[(j * cw + i) as usize];
                        let (oa, oi) = o[((j + dy) * ow + i + dx) as usize];
                        if (ga && oa && oi == Some(gi)) || (!ga && !oa) {
                            ok += 1;
                        }
                    }
                }
                best = best.max(ok);
            }
        }
        best as f64 / (cw * ch) as f64
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{self, Fixture, Rng, Spec};
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    /// Detect + snap + score per fixture, a few at a time (they are big in debug builds).
    struct Outcome {
        name: String,
        true_period: f64,
        result: Result<Grid, Refusal>,
        correct: f64,
    }

    fn run_all(specs: &[Spec], seed0: u64) -> Vec<Outcome> {
        let next = AtomicUsize::new(0);
        let out = Mutex::new(Vec::new());
        let threads = std::thread::available_parallelism().map_or(2, |n| n.get()).min(4);
        std::thread::scope(|s| {
            for _ in 0..threads {
                s.spawn(|| loop {
                    let i = next.fetch_add(1, Ordering::SeqCst);
                    if i >= specs.len() {
                        break;
                    }
                    let fx = fixtures::generate(&specs[i], seed0 + i as u64);
                    let outcome = evaluate(&fx, &DetectOptions::default());
                    out.lock().unwrap().push((i, outcome));
                });
            }
        });
        let mut v = out.into_inner().unwrap();
        v.sort_by_key(|(i, _)| *i);
        v.into_iter().map(|(_, o)| o).collect()
    }

    fn evaluate(fx: &Fixture, opts: &DetectOptions) -> Outcome {
        let result = detect(&fx.image, opts);
        let correct = match &result {
            Ok(g) => fixtures::score(&snap(&fx.image, g), &fx.truth, fx.spec.period, fx.spec.offset),
            Err(_) => 0.0,
        };
        Outcome { name: fx.spec.name.clone(), true_period: fx.spec.period, result, correct }
    }

    fn period_error(o: &Outcome) -> f64 {
        match &o.result {
            Ok(g) => g.period.iter().map(|&p| (f64::from(p) - o.true_period).abs()).fold(0.0, f64::max),
            Err(_) => f64::INFINITY,
        }
    }

    fn report(outcomes: &[Outcome]) {
        for o in outcomes {
            match &o.result {
                Ok(g) => println!(
                    "{:24} {:7.2}%  period {:.3},{:.3} (err {:.3})  {} {}x{}  capture {:.3} purity {:.3}  cands {:?}",
                    o.name, o.correct * 100.0, g.period[0], g.period[1], period_error(o), g.mode.name(), g.cells[0], g.cells[1], g.capture, g.purity, g.candidates
                ),
                Err(r) => println!("{:24} REFUSED {r} {:?}", o.name, r.grid.as_deref().map(Grid::to_json)),
            }
        }
    }

    #[test]
    fn fft_matches_dft() {
        let sig: Vec<f64> = (0..64).map(|i| ((i * 7) % 11) as f64).collect();
        let mut re = sig.clone();
        let mut im = vec![0.0; 64];
        fft(&mut re, &mut im);
        for k in [1usize, 5, 17, 31] {
            let (dr, di) = dft(&sig, k as f64 / 64.0);
            // dft() phases from position 1: rotate back by one sample.
            let (s, c) = (2.0 * PI * k as f64 / 64.0).sin_cos();
            let (er, ei) = (dr * c - di * s, dr * s + di * c);
            assert!((er - re[k]).abs() < 1e-9 && (ei - im[k]).abs() < 1e-9);
        }
    }

    #[test]
    fn fixed_cases_a_to_g_find_the_period_and_the_cells() {
        let specs = fixtures::fixed();
        let outcomes = run_all(&specs, 1000);
        report(&outcomes);
        assert_eq!(outcomes.len(), 7);
        for o in &outcomes {
            assert!(o.result.is_ok(), "{} refused", o.name);
            assert!(period_error(o) <= 0.5, "{}: period error {:.3}", o.name, period_error(o));
            assert!(o.correct >= 0.98, "{}: {:.2}% cells correct", o.name, o.correct * 100.0);
        }
    }

    #[test]
    fn randomised_fixtures_mean_and_count_at_98_percent() {
        let specs = fixtures::holdout(16, 777);
        let outcomes = run_all(&specs, 5777);
        report(&outcomes);
        assert_eq!(outcomes.len(), 16);
        let mean = outcomes.iter().map(|o| o.correct).sum::<f64>() / 16.0;
        let good = outcomes.iter().filter(|o| o.correct >= 0.98).count();
        println!("mean {:.2}%, {good}/16 at >= 98%", mean * 100.0);
        assert!(mean >= 0.98, "mean {:.2}%", mean * 100.0);
        assert!(good >= 14, "{good}/16 at >= 98%");
    }

    /// Value noise: bilinear interpolation of a random lattice, several octaves.
    fn value_noise(w: u32, h: u32, cell: f64, octaves: u32, rng: &mut Rng) -> Vec<f64> {
        let mut out = vec![0.0; (w * h) as usize];
        let mut amp = 1.0;
        let mut c = cell;
        for _ in 0..octaves {
            let gw = (f64::from(w) / c).ceil() as usize + 2;
            let gh = (f64::from(h) / c).ceil() as usize + 2;
            let lattice: Vec<f64> = (0..gw * gh).map(|_| rng.f64()).collect();
            for y in 0..h {
                for x in 0..w {
                    let (fx, fy) = (f64::from(x) / c, f64::from(y) / c);
                    let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
                    let (tx, ty) = (fx - fx.floor(), fy - fy.floor());
                    let (sx, sy) = (tx * tx * (3.0 - 2.0 * tx), ty * ty * (3.0 - 2.0 * ty));
                    let l = |i: usize, j: usize| lattice[j * gw + i];
                    let top = l(x0, y0) * (1.0 - sx) + l(x0 + 1, y0) * sx;
                    let bottom = l(x0, y0 + 1) * (1.0 - sx) + l(x0 + 1, y0 + 1) * sx;
                    out[(y * w + x) as usize] += amp * (top * (1.0 - sy) + bottom * sy);
                }
            }
            amp *= 0.5;
            c /= 2.0;
        }
        out
    }

    fn painting(w: u32, h: u32, seed: u64) -> RgbaImage {
        let mut rng = Rng::new(seed);
        let r = value_noise(w, h, 90.0, 5, &mut rng);
        let g = value_noise(w, h, 70.0, 5, &mut rng);
        let b = value_noise(w, h, 110.0, 5, &mut rng);
        RgbaImage::from_fn(w, h, |x, y| {
            let i = (y * w + x) as usize;
            let v = |n: f64| (n / 1.94 * 255.0).clamp(0.0, 255.0) as u8;
            Rgba([v(r[i]), v(g[i]), v(b[i]), 255])
        })
    }

    /// A photo-like scene: soft value-noise background, overlapping shaded discs and fine grain,
    /// drawn at 3x and downscaled with Lanczos.
    fn photo(w: u32, h: u32, seed: u64) -> RgbaImage {
        let mut rng = Rng::new(seed);
        let (bw, bh) = (w * 3, h * 3);
        let mut img = painting(bw, bh, seed ^ 0x55);
        for _ in 0..40 {
            let (cx, cy) = (rng.uniform(0.0, f64::from(bw)), rng.uniform(0.0, f64::from(bh)));
            let rad = rng.uniform(30.0, 300.0);
            let col = [rng.uniform(0.0, 255.0), rng.uniform(0.0, 255.0), rng.uniform(0.0, 255.0)];
            for y in (cy - rad).max(0.0) as u32..((cy + rad) as u32).min(bh) {
                for x in (cx - rad).max(0.0) as u32..((cx + rad) as u32).min(bw) {
                    let d = ((f64::from(x) - cx).powi(2) + (f64::from(y) - cy).powi(2)).sqrt() / rad;
                    if d <= 1.0 {
                        let shade = 1.0 - 0.6 * d * d;
                        let p = img.get_pixel_mut(x, y);
                        for (v, c) in p.0.iter_mut().zip(col) {
                            *v = (c * shade).clamp(0.0, 255.0) as u8;
                        }
                    }
                }
            }
        }
        for p in img.pixels_mut() {
            for v in p.0.iter_mut().take(3) {
                *v = (f64::from(*v) + 6.0 * rng.normal()).clamp(0.0, 255.0) as u8;
            }
        }
        image::imageops::resize(&img, w, h, image::imageops::FilterType::Lanczos3)
    }

    #[test]
    fn non_pixel_art_is_refused() {
        let gradient = RgbaImage::from_fn(640, 480, |x, y| Rgba([(x * 255 / 639) as u8, (y * 255 / 479) as u8, 128, 255]));
        let cases = [("gradient", gradient), ("painting", painting(640, 480, 11)), ("photo", photo(640, 480, 12))];
        for (name, img) in &cases {
            // precondition: the image has structure to look at (not blank)
            let (px, py) = edge_profiles(img);
            assert!(px.iter().sum::<f64>() > 0.0 && py.iter().sum::<f64>() > 0.0, "{name} is blank");
            let result = detect(img, &DetectOptions::default());
            if let Err(r) = &result {
                println!("{name}: {r} {:?}", r.grid.as_deref().map(Grid::to_json));
            }
            assert!(result.is_err(), "{name} not refused: {:?}", result.map(|g| g.to_json()));
        }
    }

    #[test]
    fn sprite_sheet_gives_the_cell_size_not_the_sprite_pitch() {
        // 16 x 16 copies of one 8 x 8-cell sprite with a 2-cell gap: pitch 10 cells = 53 px, within
        // the searched range (min(W, H) / 16 = 53), so the selection itself must reject it.
        let mut rng = Rng::new(42);
        let sprite = fixtures::make_art(12, 12, &mut rng, true);
        let (cw, ch) = (160usize, 160usize);
        let art: Vec<i16> = (0..cw * ch)
            .map(|i| {
                let (x, y) = (i % cw % 10, i / cw % 10);
                if x < 8 && y < 8 { sprite[(y + 2) * 12 + x + 2] } else { -1 }
            })
            .collect();
        assert!(art.iter().filter(|&&v| v >= 0).count() > cw * ch / 4, "sprites are mostly empty");
        let period = 5.3;
        let bx = fixtures::boundaries(cw, period, 2.0, fixtures::GridMode::Uniform, &mut rng);
        let by = fixtures::boundaries(ch, period, 2.0, fixtures::GridMode::Uniform, &mut rng);
        let img = fixtures::degrade(&fixtures::render(&art, cw, &bx, &by), 0.8, 3.0, 85, &mut rng);
        assert!(f64::from(img.width().min(img.height())) / 16.0 >= 10.0 * period - 0.5, "pitch outside the searched range");
        let grid = detect(&img, &DetectOptions::default()).expect("sprite sheet detected");
        println!("{:?}", grid.to_json()["period"]);
        for p in grid.period {
            assert!((f64::from(p) - period).abs() <= 0.5, "period {p} instead of {period}");
        }
        let truth = fixtures::truth_image(&art, cw);
        let correct = fixtures::score(&snap(&img, &grid), &truth, period, (2.0, 2.0));
        assert!(correct >= 0.98, "{:.2}% cells correct", correct * 100.0);
    }

    #[test]
    fn fixed_fractional_size_snaps_and_hardens_alpha() {
        let spec = fixtures::fixed().into_iter().find(|s| s.name.starts_with("D_alpha")).unwrap();
        let fx = fixtures::generate(&spec, 1003);
        // precondition: the source has soft alpha (neither 0 nor 255) at the sprite edges
        assert!(fx.image.pixels().any(|p| p.0[3] > 0 && p.0[3] < 255), "no soft alpha in the fixture");
        let opts = DetectOptions { size: Some(9.6), ..DetectOptions::default() };
        let grid = detect(&fx.image, &opts).expect("fixed size never refuses on confidence");
        assert!(grid.candidates.is_empty(), "no period search with a fixed size");
        for p in grid.period {
            assert!((f64::from(p) - 9.6).abs() <= 0.1, "period {p} for size 9.6");
        }
        let out = snap(&fx.image, &grid);
        assert_eq!((out.width(), out.height()), (grid.cells[0], grid.cells[1]));
        assert!(out.pixels().all(|p| p.0[3] == 0 || p.0[3] == 255), "alpha not hardened per cell");
        assert!(out.pixels().any(|p| p.0[3] == 0) && out.pixels().any(|p| p.0[3] == 255), "both kinds of cells");
        // score() counts a cell only when its alpha agrees with the truth (transparent → 0, opaque → 255)
        let correct = fixtures::score(&out, &fx.truth, spec.period, spec.offset);
        assert!(correct >= 0.98, "{:.2}% cells correct", correct * 100.0);
    }

    #[test]
    fn fixed_size_keeps_a_fractional_period() {
        let spec = fixtures::fixed().into_iter().next().unwrap(); // A_uniform_7.3
        let fx = fixtures::generate(&spec, 1000);
        let grid = detect(&fx.image, &DetectOptions { size: Some(7.3), ..DetectOptions::default() }).unwrap();
        for p in grid.period {
            assert!((f64::from(p) - 7.3).abs() <= 0.05, "period {p} for size 7.3");
        }
        let correct = fixtures::score(&snap(&fx.image, &grid), &fx.truth, spec.period, spec.offset);
        assert!(correct >= 0.98, "{:.2}% cells correct", correct * 100.0);
    }

    #[test]
    fn invalid_fixed_size_and_tiny_images_refuse() {
        let img = RgbaImage::from_pixel(64, 64, Rgba([10, 20, 30, 255]));
        for size in [0.5, f32::NAN, 65.0] {
            let r = detect(&img, &DetectOptions { size: Some(size), ..DetectOptions::default() }).unwrap_err();
            assert_eq!(r.reasons, vec![RefusalReason::InvalidSize], "size {size}");
        }
        // 64 px / 16 = 4: only a 4 px period could fit; a flat image has none.
        let r = detect(&img, &DetectOptions::default()).unwrap_err();
        assert_eq!(r.reasons, vec![RefusalReason::NoPeriod]);
        assert!(r.grid.is_none());
        let one = RgbaImage::from_pixel(1, 40, Rgba([0, 0, 0, 255]));
        assert!(detect(&one, &DetectOptions::default()).is_err());
    }

    #[test]
    fn to_json_carries_the_measurements() {
        let grid = Grid {
            period: [7.3, 7.25],
            offset: [-1.5, 0.25],
            mode: Mode::Elastic,
            bx: vec![-1.5, 5.75, 13.0],
            by: vec![0.25, 7.5],
            cells: [2, 1],
            capture: 0.31234,
            purity: 0.95,
            coherence: [0.4, 0.5],
            candidates: vec![7.3, 14.6],
        };
        let v = grid.to_json();
        assert_eq!(v["mode"], "elastic");
        assert_eq!(v["cells"], json!([2, 1]));
        assert_eq!(v["period"], json!([7.3, 7.25]));
        assert_eq!(v["capture"], json!(0.3123));
        assert_eq!(v["bx"], json!([-1.5, 5.75, 13.0]));
        assert_eq!(v["candidates"], json!([7.3, 14.6]));
        let refusal = Refusal { reasons: vec![RefusalReason::LowCapture, RefusalReason::LowPurity], grid: Some(Box::new(grid)) };
        let r = refusal.to_json();
        assert_eq!(r["reasons"], json!(["low_capture", "low_purity"]));
        assert_eq!(r["measurements"]["purity"], json!(0.95));
        let text = refusal.to_string();
        assert!(text.contains("capture 0.312") && text.contains("purity 0.950"), "{text}");
    }

    /// Unseen randomised sets for checking a change against overfitting: `SEED=4242 cargo test -p
    /// codex-img-core pixel_grid::tests::holdout_any_seed -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn holdout_any_seed() {
        let seed: u64 = std::env::var("SEED").expect("SEED").parse().expect("SEED is a number");
        let outcomes = run_all(&fixtures::holdout(16, seed), 5000 + seed);
        report(&outcomes);
        let mean = outcomes.iter().map(|o| o.correct).sum::<f64>() / 16.0;
        let good = outcomes.iter().filter(|o| o.correct >= 0.98).count();
        let within = outcomes.iter().filter(|o| period_error(o) <= 0.5).count();
        println!("mean {:.2}%, {good}/16 at >= 98%, {within}/16 period within 0.5", mean * 100.0);
    }

    /// Release-mode timing for a ~1 MP fixture: `cargo test --release -p codex-img-core
    /// pixel_grid::tests::timing -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn timing_one_megapixel() {
        let spec = Spec { name: "timing".into(), cells: (150, 120), period: 7.3, offset: (3.0, 3.0), mode: fixtures::GridMode::Jitter, transparent: false, blur: 0.8, noise: 4.0, jpeg: 80 };
        let fx = fixtures::generate(&spec, 9);
        let t = std::time::Instant::now();
        let grid = detect(&fx.image, &DetectOptions::default()).unwrap();
        let t_detect = t.elapsed();
        let out = snap(&fx.image, &grid);
        println!("{}x{} ({:.2} MP): detect {:?}, detect+snap {:?}, {}x{} cells", fx.image.width(), fx.image.height(), f64::from(fx.image.width() * fx.image.height()) / 1e6, t_detect, t.elapsed(), out.width(), out.height());
    }
}
