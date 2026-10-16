//! The CPU renderers: the mid-depth f64 SIMD tier, the deep perturbation
//! renderer with its glitch-retry loop, and the experimental BLA engine.
//!
//! None of these is a performance path. They are the reference the GPU tiers
//! were validated against, and the fallback when a pixel cannot be resolved
//! any other way.

use rosie_mandlebrot_core::error::MbResult;
use rosie_mandlebrot_core::precision::Tier;
use rosie_mandlebrot_core::types::PixelOut;
use rosie_mandlebrot_ref::{
    bla_iterate, build_bla_table, compute_reference, BlaResult, RefRequest,
};
use crate::{frame::*, perturb::*};
use num_complex::Complex64;
use rayon::prelude::*;


/// CPU BLA deep-tier engine. Builds the BLA table from the (cached) reference
/// orbit, then walks each pixel with iteration-skipping BLA (validated exact vs
/// plain perturbation in `reference::bla_iterate`), falling back to rug only for
/// the rare Pauldelbrot glitch. Far fewer iterations ⇒ far less rounding-error
/// accumulation ⇒ far fewer glitches ⇒ much less rug than plain perturbation at
/// the deep tail. Only called for spans below `BLA_SPAN_THRESHOLD`.
pub(crate) fn render_deep_cpu_bla(req: &FrameRequest) -> MbResult<FrameResult> {
    use std::sync::atomic::Ordering;
    let total = (req.width * req.height) as usize;
    let aspect = req.width as f64 / req.height as f64;
    let px_dx = req.span * aspect / req.width as f64;
    let px_dy = req.span / req.height as f64;
    let half_w = req.width as f64 * 0.5;
    let half_h = req.height as f64 * 0.5;
    let max_iter = req.max_iter;

    // Reference orbit at the frame center (cached across frames), then the BLA
    // table for this span.
    let reference = cached_reference(&RefRequest {
        want_sa: false, // CPU kernels do not use series approximation
        center_re: req.center_re.clone(),
        center_im: req.center_im.clone(),
        max_iter,
        max_delta: req.span.max(1e-300),
    });
    let orbit = std::sync::Arc::new(reference.orbit_f64);
    // |dc| never exceeds the frame's half-diagonal.
    let max_delta = 0.5 * (req.span * aspect).hypot(req.span);
    let table = build_bla_table(&orbit, max_delta.max(1e-300), BLA_EPSILON);

    // First pass: BLA walk over every pixel.
    let stop = req.stop_flag.clone();
    let results: Vec<(PixelOut, bool)> = (0..total)
        .into_par_iter()
        .map(|i| {
            if let Some(s) = &stop {
                if s.load(Ordering::Relaxed) {
                    return (PixelOut { iter: max_iter, smooth_val: 0.0 }, false);
                }
            }
            let x = (i as u32) % req.width;
            let y = (i as u32) / req.width;
            let dc = Complex64::new(
                (x as f64 - half_w) * px_dx,
                (y as f64 - half_h) * px_dy,
            );
            match bla_iterate(&table, &orbit, dc, max_iter) {
                BlaResult::Escaped { iter, smooth } => {
                    (PixelOut { iter, smooth_val: smooth as f32 }, false)
                }
                BlaResult::Inside => (PixelOut { iter: max_iter, smooth_val: 0.0 }, false),
                BlaResult::NeedsRug => {
                    (PixelOut { iter: max_iter, smooth_val: f32::NAN }, true)
                }
            }
        })
        .collect();
    req.bump_progress(total as u64);

    let mut pixels: Vec<PixelOut> = Vec::with_capacity(total);
    let mut needs_rug: Vec<usize> = Vec::new();
    for (i, (px, rug)) in results.into_iter().enumerate() {
        pixels.push(px);
        if rug {
            needs_rug.push(i);
        }
    }
    let rug_pixels = needs_rug.len() as u32;

    if req.stop_requested() {
        return Err(rosie_mandlebrot_core::error::MbError::Cancelled);
    }

    // Rug fallback for the (rare) glitched pixels.
    if !needs_rug.is_empty() {
        let rug_prec_bits = rosie_mandlebrot_core::precision::rug_bits_for_pixel(req.span);
        req.reset_progress(needs_rug.len() as u64);
        let stop2 = req.stop_flag.clone();
        let rug_results: Vec<(usize, PixelOut)> = needs_rug
            .par_iter()
            .map(|&i| {
                if let Some(s) = &stop2 {
                    if s.load(Ordering::Relaxed) {
                        return (i, PixelOut { iter: max_iter, smooth_val: 0.0 });
                    }
                }
                let x = (i as u32) % req.width;
                let y = (i as u32) / req.width;
                let delta_re = (x as f64 - half_w) * px_dx;
                let delta_im = (y as f64 - half_h) * px_dy;
                let px = iterate_single_pixel_rug(
                    &req.center_re,
                    &req.center_im,
                    delta_re,
                    delta_im,
                    max_iter,
                    rug_prec_bits,
                );
                req.bump_progress(1);
                (i, px)
            })
            .collect();
        for (i, px) in rug_results {
            pixels[i] = px;
        }
    }

    if req.stop_requested() {
        return Err(rosie_mandlebrot_core::error::MbError::Cancelled);
    }

    let glitch_count = count_glitches(&pixels);
    Ok(FrameResult {
        frame_idx: req.frame_idx,
        width: req.width,
        height: req.height,
        max_iter,
        tier: Tier::DeepPerturb,
        pixels,
        glitch_count: glitch_count as u32,
        rug_pixels,
        engine_used: "cpu_bla",
    })
}

/// Mariani-Silver tile size (pixels). 32 is a good balance: small enough that
/// thin features can't easily hide entirely in the interior of a tile (and
/// escape the perimeter check); large enough that inside-set tiles save
/// (32² - 124) ≈ 900 pixel-iterations each.
pub(crate) const MS_TILE: u32 = 32;


/// CPU f64 escape-time with **Mariani-Silver tile shortcut** and SIMD interior.
///
/// For each 32×32 tile:
/// 1. Iterate the perimeter pixels (124 of 1024) — scalar with periodicity.
/// 2. If ALL perimeter pixels hit `max_iter` (entirely inside-set), fill the
///    interior with `max_iter` directly. **No iteration on interior pixels.**
///    Mathematically exact for the Mandelbrot set because level sets are
///    connected (a closed inside-set perimeter encloses an inside-set
///    interior).
/// 3. Otherwise, iterate the interior in SIMD f64x4 chunks.
///
/// For seahorse-valley deep zooms (where ~70-80% of the image is inside-set),
/// this skips iteration on ~70% of pixels — typically 3-5× speedup of the
/// mid_f64 tier on top of SIMD.
pub(crate) fn render_mid_f64(req: &FrameRequest) -> MbResult<FrameResult> {
    let total = (req.width * req.height) as usize;
    let aspect = req.width as f64 / req.height as f64;
    let span_x = req.span * aspect;
    let span_y = req.span;
    let px_dx = span_x / req.width as f64;
    let px_dy = span_y / req.height as f64;
    let half_w = req.width as f64 * 0.5;
    let half_h = req.height as f64 * 0.5;
    let max_iter = req.max_iter;
    let cre = parse_f64(&req.center_re);
    let cim = parse_f64(&req.center_im);

    let tiles_x = req.width.div_ceil(MS_TILE);
    let tiles_y = req.height.div_ceil(MS_TILE);
    let total_tiles = (tiles_x * tiles_y) as usize;

    // Each tile produces (origin_x, origin_y, width, height, Vec<PixelOut>).
    // We then stitch into the final frame buffer.
    type TileOut = (u32, u32, u32, u32, Vec<PixelOut>);
    let tile_results: Vec<TileOut> = (0..total_tiles)
        .into_par_iter()
        .map(|tidx| {
            let tx = (tidx as u32) % tiles_x;
            let ty = (tidx as u32) / tiles_x;
            let x_start = tx * MS_TILE;
            let y_start = ty * MS_TILE;
            let x_end = (x_start + MS_TILE).min(req.width);
            let y_end = (y_start + MS_TILE).min(req.height);
            let tw = x_end - x_start;
            let th = y_end - y_start;
            let mut tile = vec![PixelOut::default(); (tw * th) as usize];

            // Helper: compute one pixel at tile-local (dx, dy).
            let pixel_at = |dx: u32, dy: u32| -> PixelOut {
                let x = x_start + dx;
                let y = y_start + dy;
                let c_re = cre + (x as f64 - half_w) * px_dx;
                let c_im = cim + (y as f64 - half_h) * px_dy;
                iterate_escape_time_f64(c_re, c_im, max_iter)
            };

            let mut perimeter_all_inside = true;

            // Top row
            for dx in 0..tw {
                let p = pixel_at(dx, 0);
                tile[dx as usize] = p;
                if p.iter < max_iter { perimeter_all_inside = false; }
            }
            // Bottom row (if there is a distinct one)
            if th > 1 {
                for dx in 0..tw {
                    let p = pixel_at(dx, th - 1);
                    tile[((th - 1) * tw + dx) as usize] = p;
                    if p.iter < max_iter { perimeter_all_inside = false; }
                }
            }
            // Left/right columns excluding corners
            for dy in 1..th.saturating_sub(1) {
                let pl = pixel_at(0, dy);
                tile[(dy * tw) as usize] = pl;
                if pl.iter < max_iter { perimeter_all_inside = false; }
                if tw > 1 {
                    let pr = pixel_at(tw - 1, dy);
                    tile[(dy * tw + tw - 1) as usize] = pr;
                    if pr.iter < max_iter { perimeter_all_inside = false; }
                }
            }

            if perimeter_all_inside {
                // Mariani-Silver shortcut: fill interior with inside-set.
                let inside = PixelOut { iter: max_iter, smooth_val: 0.0 };
                for dy in 1..th.saturating_sub(1) {
                    for dx in 1..tw.saturating_sub(1) {
                        tile[(dy * tw + dx) as usize] = inside;
                    }
                }
            } else if th >= 3 && tw >= 3 {
                // Iterate interior pixels in SIMD chunks of 4.
                // Build list of (dx, dy) for all interior pixels.
                let interior_count = ((tw - 2) * (th - 2)) as usize;
                let mut interior_coords: Vec<(u32, u32)> = Vec::with_capacity(interior_count);
                for dy in 1..(th - 1) {
                    for dx in 1..(tw - 1) {
                        interior_coords.push((dx, dy));
                    }
                }
                let mut i = 0;
                while i + 4 <= interior_coords.len() {
                    let mut c_re_arr = [0.0_f64; 4];
                    let mut c_im_arr = [0.0_f64; 4];
                    for lane in 0..4 {
                        let (dx, dy) = interior_coords[i + lane];
                        c_re_arr[lane] = cre + ((x_start + dx) as f64 - half_w) * px_dx;
                        c_im_arr[lane] = cim + ((y_start + dy) as f64 - half_h) * px_dy;
                    }
                    let results = iterate_escape_time_f64x4(c_re_arr, c_im_arr, max_iter);
                    for lane in 0..4 {
                        let (dx, dy) = interior_coords[i + lane];
                        tile[(dy * tw + dx) as usize] = results[lane];
                    }
                    i += 4;
                }
                // Remainder (0-3 pixels) — scalar.
                while i < interior_coords.len() {
                    let (dx, dy) = interior_coords[i];
                    tile[(dy * tw + dx) as usize] = pixel_at(dx, dy);
                    i += 1;
                }
            }
            // For tiny edge tiles (th<3 or tw<3), the perimeter pass already
            // covered everything, so no interior step is needed.

            req.bump_progress((tw * th) as u64);
            (x_start, y_start, tw, th, tile)
        })
        .collect();

    // Stitch tile results into the frame buffer.
    let mut pixels = vec![PixelOut::default(); total];
    for (x_start, y_start, tw, th, tile) in tile_results {
        for dy in 0..th {
            for dx in 0..tw {
                let frame_idx = ((y_start + dy) * req.width + (x_start + dx)) as usize;
                pixels[frame_idx] = tile[(dy * tw + dx) as usize];
            }
        }
    }

    Ok(FrameResult {
        frame_idx: req.frame_idx,
        width: req.width,
        height: req.height,
        max_iter,
        tier: Tier::MidF64,
        pixels,
        glitch_count: 0,
        rug_pixels: 0,
        engine_used: "mid_f64",
    })
}


/// Pure-CPU hybrid perturbation for the deep tier:
///
/// 1. **Glitch retry with new reference** before falling back to rug. Picks
///    the centroid (well — highest-iter, far from prior seeds) of remaining
///    NeedsRug pixels as a new reference, recomputes the reference orbit
///    there, re-iterates *only those pixels* against the new reference.
///    Typically avoids 10-100× the rug calls a per-pixel fallback would need.
/// 2. **Reference orbit terminates early** when |z|>2 (already in `compute_reference`).
/// 3. **Smooth iter formula** avoids a sqrt (uses `ln(mag_sq)*0.5` not `ln(norm)`).
/// 4. **f32 smooth values, no Float allocations per pixel.**
/// 5. **Rayon parallel** with the `--cpu-threads` pool.
pub(crate) fn render_deep_cpu(req: &FrameRequest, glitch_threshold: usize) -> MbResult<FrameResult> {
    let total = (req.width * req.height) as usize;
    let aspect = req.width as f64 / req.height as f64;
    let span_x = req.span * aspect;
    let span_y = req.span;
    let px_dx = span_x / req.width as f64;
    let px_dy = span_y / req.height as f64;
    let half_w = req.width as f64 * 0.5;
    let half_h = req.height as f64 * 0.5;
    let max_iter = req.max_iter;
    let center_cre = parse_f64(&req.center_re);
    let center_cim = parse_f64(&req.center_im);

    // `pixels` accumulates the best-known result per pixel.
    // `needs_more` flags pixels still glitched (NeedsRug from the f64 pass)
    // so retries only re-iterate them, not the whole frame.
    let mut pixels: Vec<PixelOut> = vec![
        PixelOut { iter: 0, smooth_val: f32::NAN }; // NaN = "not yet resolved"
        total
    ];
    let mut needs_more = vec![true; total];

    let mut reference_re = req.center_re.clone();
    let mut reference_im = req.center_im.clone();
    let mut ref_offset_x = 0.0_f64;
    let mut ref_offset_y = 0.0_f64;
    let mut tried_seeds: Vec<(u32, u32)> = Vec::new();

    for retry in 0..=req.max_retries {
        if req.stop_requested() {
            return Err(rosie_mandlebrot_core::error::MbError::Cancelled);
        }
        // On retries (retry > 0), pick a new reference inside the still-
        // glitched region and recompute the reference orbit there.
        if retry > 0 {
            let seed = pick_seed_among_unresolved(req, &needs_more, &tried_seeds);
            let Some((new_re_str, new_im_str, seed_px)) = seed else { break };
            tried_seeds.push(seed_px);
            ref_offset_x = parse_f64(&new_re_str) - center_cre;
            ref_offset_y = parse_f64(&new_im_str) - center_cim;
            reference_re = new_re_str;
            reference_im = new_im_str;
        }

        let reference = compute_reference(&RefRequest {
            want_sa: false,
            center_re: reference_re.clone(),
            center_im: reference_im.clone(),
            max_iter,
            max_delta: req.span.max(1e-300),
        });
        let ref_orbit = std::sync::Arc::new(reference.orbit_f64);
        let ref_len = ref_orbit.len();

        // First pass (retry == 0): SIMD-batched over the full frame in
        // chunks of 4 pixels. Most pixels resolve here.
        // Retry passes: scalar over just the unresolved pixels (small set,
        // not worth batching).
        let resolved_this_pass = if retry == 0 {
            // SIMD path: process all pixels in 4-lane chunks.
            let chunks = total.div_ceil(4);
            let stop = req.stop_flag.clone();
            let chunk_results: Vec<[Option<(PixelOut, bool)>; 4]> = (0..chunks)
                .into_par_iter()
                .map(|chunk_idx| {
                    // Each worker checks the shared stop flag once per chunk
                    // (every 4 pixels). When the user hits Ctrl+C the remaining
                    // chunks short-circuit to a sentinel and the par_iter
                    // collapses in milliseconds instead of minutes.
                    if let Some(s) = &stop {
                        if s.load(std::sync::atomic::Ordering::Relaxed) {
                            return [
                                Some((PixelOut { iter: max_iter, smooth_val: 0.0 }, false)),
                                Some((PixelOut { iter: max_iter, smooth_val: 0.0 }, false)),
                                Some((PixelOut { iter: max_iter, smooth_val: 0.0 }, false)),
                                Some((PixelOut { iter: max_iter, smooth_val: 0.0 }, false)),
                            ];
                        }
                    }
                    let base_i = chunk_idx * 4;
                    let mut delta_re_arr = [0.0_f64; 4];
                    let mut delta_im_arr = [0.0_f64; 4];
                    for lane in 0..4 {
                        let i = base_i + lane;
                        if i < total {
                            let x = (i as u32) % req.width;
                            let y = (i as u32) / req.width;
                            delta_re_arr[lane] =
                                (x as f64 - half_w) * px_dx - ref_offset_x;
                            delta_im_arr[lane] =
                                (y as f64 - half_h) * px_dy - ref_offset_y;
                        }
                    }
                    let lane_results = iterate_perturbation_f64x4(
                        &ref_orbit, ref_len, delta_re_arr, delta_im_arr, max_iter,
                    );
                    let mut out = [None, None, None, None];
                    for lane in 0..4 {
                        let i = base_i + lane;
                        if i < total {
                            match &lane_results[lane] {
                                F64Result::Done(px) => out[lane] = Some((*px, false)),
                                F64Result::NeedsRug => out[lane] = Some((
                                    PixelOut { iter: max_iter, smooth_val: f32::NAN },
                                    true,
                                )),
                            }
                        }
                    }
                    out
                })
                .collect();

            let mut count = 0usize;
            for (chunk_idx, chunk) in chunk_results.iter().enumerate() {
                let base_i = chunk_idx * 4;
                // Indexing by `lane` deliberately: lane N of the SIMD batch is
                // chunk[N], and that correspondence is the point.
                #[allow(clippy::needless_range_loop)]
                for lane in 0..4 {
                    let i = base_i + lane;
                    if i >= total { break; }
                    if let Some((px, glitched)) = chunk[lane] {
                        pixels[i] = px;
                        needs_more[i] = glitched;
                        if !glitched { count += 1; }
                    }
                }
            }
            count
        } else {
            // Retry path: scalar perturbation for just the unresolved pixels.
            let needs_more_snapshot = needs_more.clone();
            let stop = req.stop_flag.clone();
            let updates: Vec<(usize, PixelOut, bool)> = (0..total)
                .into_par_iter()
                .filter_map(|i| {
                    if let Some(s) = &stop {
                        if s.load(std::sync::atomic::Ordering::Relaxed) {
                            return None;
                        }
                    }
                    if !needs_more_snapshot[i] { return None; }
                    let x = (i as u32) % req.width;
                    let y = (i as u32) / req.width;
                    let delta_re = (x as f64 - half_w) * px_dx - ref_offset_x;
                    let delta_im = (y as f64 - half_h) * px_dy - ref_offset_y;
                    match iterate_perturbation_f64(
                        &ref_orbit, ref_len, delta_re, delta_im, max_iter,
                    ) {
                        F64Result::Done(px) => Some((i, px, false)),
                        F64Result::NeedsRug => Some((
                            i,
                            PixelOut { iter: max_iter, smooth_val: f32::NAN },
                            true,
                        )),
                    }
                })
                .collect();
            let mut count = 0usize;
            for (i, px, still_glitched) in updates {
                pixels[i] = px;
                needs_more[i] = still_glitched;
                if !still_glitched { count += 1; }
            }
            count
        };

        let remaining = needs_more.iter().filter(|&&b| b).count();
        tracing::debug!(
            "frame {} retry {}: resolved {} pixels this pass, {} still glitched",
            req.frame_idx, retry, resolved_this_pass, remaining
        );
        if remaining <= glitch_threshold {
            break;
        }
        if resolved_this_pass == 0 && retry > 0 {
            break;
        }
    }

    // Final rug fallback for the pixels even retry-with-new-reference
    // couldn't resolve. These should be very few (≪0.1% in practice).
    let still_unresolved: Vec<usize> = needs_more
        .iter()
        .enumerate()
        .filter_map(|(i, &b)| if b { Some(i) } else { None })
        .collect();
    let rug_pixels = still_unresolved.len() as u32;

    if req.stop_requested() {
        return Err(rosie_mandlebrot_core::error::MbError::Cancelled);
    }

    if !still_unresolved.is_empty() {
        let rug_prec_bits = rosie_mandlebrot_core::precision::rug_bits_for_pixel(req.span);
        req.reset_progress(still_unresolved.len() as u64);
        let stop = req.stop_flag.clone();
        let rug_results: Vec<(usize, PixelOut)> = still_unresolved
            .par_iter()
            .map(|&i| {
                if let Some(s) = &stop {
                    if s.load(std::sync::atomic::Ordering::Relaxed) {
                        return (i, PixelOut { iter: max_iter, smooth_val: 0.0 });
                    }
                }
                let x = (i as u32) % req.width;
                let y = (i as u32) / req.width;
                let delta_re = (x as f64 - half_w) * px_dx;
                let delta_im = (y as f64 - half_h) * px_dy;
                let px = iterate_single_pixel_rug(
                    &req.center_re,
                    &req.center_im,
                    delta_re,
                    delta_im,
                    max_iter,
                    rug_prec_bits,
                );
                req.bump_progress(1);
                (i, px)
            })
            .collect();
        tracing::debug!(
            "frame {}: {} pixels needed rug final fallback",
            req.frame_idx,
            rug_results.len()
        );
        for (i, px) in rug_results {
            pixels[i] = px;
        }
    }

    let glitch_count = count_glitches(&pixels);
    Ok(FrameResult {
        frame_idx: req.frame_idx,
        width: req.width,
        height: req.height,
        max_iter,
        tier: Tier::DeepPerturb,
        pixels,
        glitch_count: glitch_count as u32,
        rug_pixels,
        engine_used: "cpu_f64",
    })
}

/// Pick a seed pixel for a CPU retry pass from the set of currently-unresolved
/// pixels. Picks the unresolved pixel farthest from any tried seed (or first
/// if none tried). Returns its (re, im) coords as strings + the pixel coords.
pub(crate) fn pick_seed_among_unresolved(
    req: &FrameRequest,
    needs_more: &[bool],
    tried_seeds: &[(u32, u32)],
) -> Option<(String, String, (u32, u32))> {
    let min_distance_sq: u64 = {
        let d = req.width.min(req.height) / 8;
        (d as u64) * (d as u64)
    };

    // Find a needs_more pixel far from all tried seeds.
    let mut chosen: Option<(u32, u32)> = None;
    for (i, &flag) in needs_more.iter().enumerate() {
        if !flag {
            continue;
        }
        let x = (i as u32) % req.width;
        let y = (i as u32) / req.width;
        let too_close = tried_seeds.iter().any(|&(tx, ty)| {
            let dx = x as i64 - tx as i64;
            let dy = y as i64 - ty as i64;
            ((dx * dx + dy * dy) as u64) < min_distance_sq
        });
        if !too_close {
            chosen = Some((x, y));
            break;
        }
    }
    let (cx, cy) = chosen?;

    let aspect = req.width as f64 / req.height as f64;
    let span_x = req.span * aspect;
    let px_dx = span_x / req.width as f64;
    let px_dy = req.span / req.height as f64;
    let half_w = req.width as f64 * 0.5;
    let half_h = req.height as f64 * 0.5;
    let cre = parse_f64(&req.center_re);
    let cim = parse_f64(&req.center_im);
    let new_re = cre + (cx as f64 - half_w) * px_dx;
    let new_im = cim + (cy as f64 - half_h) * px_dy;
    Some((format!("{:.20}", new_re), format!("{:.20}", new_im), (cx, cy)))
}
