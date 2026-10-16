//! The GPU path: one compute dispatch per 512-pixel tile, with the reference
//! orbit uploaded as a storage buffer and the results read back and stitched
//! into a single frame. This is where essentially all the work happens on a
//! real run — the CPU paths exist for correctness, not throughput.

use rosie_mandlebrot_core::error::{MbError, MbResult};
use rosie_mandlebrot_core::precision::Tier;
use rosie_mandlebrot_core::tiling::tiles_for;
use rosie_mandlebrot_core::types::{
    f64_to_ds, f64_to_fe, PixelOut, RefOrbitEntry, RefOrbitEntryDs, RefOrbitEntryF64, SaCoeff,
    Uniforms, WORKGROUP,
};
use rosie_mandlebrot_ref::{
    compute_reference,
    sa_as_gpu_coeffs, RefRequest, ReferenceData,
};
use crate::gpu::{readback, GpuCtx};
use crate::{frame::*, perturb::*};
use bytemuck::cast_slice;
use rayon::prelude::*;
use wgpu::util::DeviceExt;


/// GPU perturbation + parallel rug fallback for glitched pixels. The shader
/// flags glitches by writing NaN into `smooth_val` (see
/// [shaders/mandelbrot_perturb.wgsl]); we then rug-iterate just those pixels.
pub(crate) fn render_deep_gpu(gpu: &GpuCtx, req: &FrameRequest) -> MbResult<FrameResult> {
    let mut pixels = dispatch_pass(gpu, req, Tier::DeepPerturb, &req.center_re, &req.center_im)?;

    // One pass over the frame, not two. With rebasing this vector is almost
    // always empty, so a second full scan (`count_glitches`) would be 16.7M
    // reads at 4K to recount what this pass already knows.
    let glitched: Vec<usize> = pixels
        .iter()
        .enumerate()
        .filter_map(|(i, p)| if p.smooth_val.is_nan() { Some(i) } else { None })
        .collect();
    let rug_pixels = glitched.len() as u32;

    if !glitched.is_empty() {
        let aspect = req.width as f64 / req.height as f64;
        let px_dx = req.span * aspect / req.width as f64;
        let px_dy = req.span / req.height as f64;
        let half_w = req.width as f64 * 0.5;
        let half_h = req.height as f64 * 0.5;
        let rug_prec_bits = rosie_mandlebrot_core::precision::rug_bits_for_pixel(req.span);
        // Switch the live bar over to rug-fallback work, which is the part of
        // a deep frame that actually takes minutes. After GPU the bar would
        // otherwise sit at "1166400/1166400 px" the whole time the rug loop
        // grinds — useless. Reset and retarget on the glitched count.
        req.reset_progress(glitched.len() as u64);
        let stop = req.stop_flag.clone();
        let rug_results: Vec<(usize, PixelOut)> = glitched
            .par_iter()
            .map(|&i| {
                // Cheap atomic load — when the user hits Ctrl+C, every
                // remaining pixel sees the flag and short-circuits with a
                // sentinel (NaN) instead of grinding through more rug iter.
                if let Some(s) = &stop {
                    if s.load(std::sync::atomic::Ordering::Relaxed) {
                        return (i, PixelOut { iter: req.max_iter, smooth_val: 0.0 });
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
                    req.max_iter,
                    rug_prec_bits,
                );
                req.bump_progress(1);
                (i, px)
            })
            .collect();
        if req.stop_requested() {
            return Err(rosie_mandlebrot_core::error::MbError::Cancelled);
        }
        tracing::debug!(
            "frame {}: GPU deep flagged {} glitched pixels; rug fallback resolved them",
            req.frame_idx,
            rug_results.len()
        );
        for (i, px) in rug_results {
            pixels[i] = px;
        }
    }

    // Any pixel the rug fallback could not resolve is still NaN. Everything
    // else was already counted above, so recount only the (almost always empty)
    // fallback set rather than rescanning the whole frame.
    let glitch_count = glitched
        .iter()
        .filter(|&&i| pixels[i].smooth_val.is_nan())
        .count();
    let engine_used = match rosie_mandlebrot_core::precision::effective_deep_engine(req.span, req.deep_engine) {
        rosie_mandlebrot_core::config::DeepEngine::GpuF32 => "gpu_f32",
        rosie_mandlebrot_core::config::DeepEngine::GpuF64 => "gpu_f64",
        rosie_mandlebrot_core::config::DeepEngine::GpuFe => "gpu_fe",
        _ => "gpu_ds",
    };
    Ok(FrameResult {
        frame_idx: req.frame_idx,
        width: req.width,
        height: req.height,
        max_iter: req.max_iter,
        tier: Tier::DeepPerturb,
        pixels,
        glitch_count: glitch_count as u32,
        rug_pixels,
        engine_used,
    })
}


/// Run a single GPU dispatch pass with the given reference center, return
/// the full frame's pixel buffer. For shallow/medium tier the "reference"
/// is just the center coordinates used to set up uniforms; for deep tier
/// the reference is iterated by `compute_reference()` for perturbation.
pub(crate) fn dispatch_pass(
    gpu: &GpuCtx,
    req: &FrameRequest,
    tier: Tier,
    ref_re: &str,
    ref_im: &str,
) -> MbResult<Vec<PixelOut>> {
    let aspect = req.width as f64 / req.height as f64;
    let span_y = req.span;
    let span_x = req.span * aspect;
    let px_dx = span_x / req.width as f64;
    let px_dy = span_y / req.height as f64;

    let total = (req.width * req.height) as usize;
    let mut out_pixels: Vec<PixelOut> = vec![PixelOut::default(); total];
    let tiles = tiles_for(req.width, req.height);

    let reference: Option<ReferenceData> = if tier == Tier::DeepPerturb {
        // Use the cross-frame cache only when ref center == frame center
        // (i.e. no retry shift). The CPU-fallback retry path computes its
        // own reference uncached, since shifting the center every retry
        // would just thrash the cache.
        let request = RefRequest {
            // Only the legacy gpu-f32 shader reads sa_coeffs, and even there the
            // bootstrap is disabled (sa_terms = 0). Everything else would pay a
            // full orbit walk per frame to build a buffer nothing samples.
            want_sa: matches!(
                rosie_mandlebrot_core::precision::effective_deep_engine(req.span, req.deep_engine),
                rosie_mandlebrot_core::config::DeepEngine::GpuF32
            ),
            center_re: ref_re.to_string(),
            center_im: ref_im.to_string(),
            max_iter: req.max_iter,
            max_delta: req.span.max(1e-300),
        };
        let data = timed_orbit(|| {
            if ref_re == req.center_re && ref_im == req.center_im {
                cached_reference(&request)
            } else {
                compute_reference(&request)
            }
        });
        // Every GPU kernel but gpu-f64 reads the orbit as f32.
        let f32_orbit = !matches!(
            rosie_mandlebrot_core::precision::effective_deep_engine(req.span, req.deep_engine),
            rosie_mandlebrot_core::config::DeepEngine::GpuF64
        );
        if let Some(n) = data.f32_orbit_limit(req.span).filter(|_| f32_orbit) {
            return Err(MbError::BeyondEngine(format!(
                "at span {:.3e} the reference orbit passes within {:.1e} of zero at iteration {n}, below what the GPU's f32 orbit can hold",
                req.span,
                10f64.powf(data.tiny.iter().find(|t| t.n == n).map_or(f64::NEG_INFINITY, |t| t.log10_z)),
            )));
        }
        Some(data)
    } else {
        None
    };

    // Offset of reference from frame center, in complex plane units. For the
    // first pass (reference == frame center) this is zero. For retry passes
    // where we shift the reference into a glitched region, this lets us
    // recompute delta_c relative to the new reference rather than the frame
    // center — which is what the perturbation shader expects.
    let (ref_offset_x, ref_offset_y) = if tier == Tier::DeepPerturb {
        if ref_re == req.center_re && ref_im == req.center_im {
            // The live path. Taking the difference of two f64 parses of the same
            // 30+ digit string would be meaningless at deep zoom — both parse to
            // the same f64 and the true offset is far below what f64 can
            // represent. So the offset is zero by construction.
            (0.0_f64, 0.0_f64)
        } else {
            // Retry path only, where the reference is deliberately shifted into
            // a glitched region by a distance that IS representable in f64.
            let cre = parse_f64(&req.center_re);
            let cim = parse_f64(&req.center_im);
            let rre = parse_f64(ref_re);
            let rim = parse_f64(ref_im);
            (rre - cre, rim - cim)
        }
    } else {
        (0.0_f64, 0.0_f64)
    };

    // The default `gpu-ds` engine auto-upgrades to DS-floatexp (`gpu-fe`) once
    // the span is deep enough that DS would underflow; explicit engines pass
    // through unchanged. Span is constant across tiles, so resolve once here.
    let eff_engine = rosie_mandlebrot_core::precision::effective_deep_engine(req.span, req.deep_engine);

    // Time only the GPU tile loop (submit + blocking readback) for the CPU-vs-GPU
    // work split; the CPU reference orbit above is deliberately excluded.
    let gpu_t0 = std::time::Instant::now();

    // The reference orbit and the SA coefficients are identical for every tile
    // in a frame, so they are built and uploaded once here and kept alive for
    // the whole loop, rather than re-encoding the orbit (up to ~120k entries)
    // and pushing it across the bus once per tile. Deliberately inside the
    // timed region - it is GPU work.
    let (frame_ref_buf, frame_sa_buf) = if tier == Tier::DeepPerturb {
        let r = reference.as_ref().expect("reference computed for deep tier");
        // Upload the reference orbit in the format the selected engine's shader
        // expects: DS (hi/lo per component) for gpu-ds AND gpu-fe (the orbit is
        // O(1) so DS suffices for both), f64 for gpu-f64 (if compiled), f32
        // otherwise.
        let ref_buf = match eff_engine {
            rosie_mandlebrot_core::config::DeepEngine::GpuDs | rosie_mandlebrot_core::config::DeepEngine::GpuFe => {
                let ds: Vec<RefOrbitEntryDs> = r
                    .orbit_f64
                    .iter()
                    .map(|e| {
                        let (re_hi, re_lo) = f64_to_ds(e.z_re);
                        let (im_hi, im_lo) = f64_to_ds(e.z_im);
                        RefOrbitEntryDs {
                            z_re_hi: re_hi,
                            z_re_lo: re_lo,
                            z_im_hi: im_hi,
                            z_im_lo: im_lo,
                        }
                    })
                    .collect();
                gpu.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("ref-orbit-ds"),
                    contents: cast_slice::<RefOrbitEntryDs, u8>(&ds),
                    usage: wgpu::BufferUsages::STORAGE,
                })
            }
            rosie_mandlebrot_core::config::DeepEngine::GpuF64 if gpu.pipelines.has_f64_deep() => {
                gpu.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                    label: Some("ref-orbit-f64"),
                    contents: cast_slice::<RefOrbitEntryF64, u8>(&r.orbit_f64),
                    usage: wgpu::BufferUsages::STORAGE,
                })
            }
            _ => gpu.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("ref-orbit"),
                contents: cast_slice::<RefOrbitEntry, u8>(&r.orbit),
                usage: wgpu::BufferUsages::STORAGE,
            }),
        };
        let sa_arr = sa_as_gpu_coeffs(&r.sa);
        let sa_buf = gpu.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("sa-coeffs"),
            contents: cast_slice::<SaCoeff, u8>(&sa_arr),
            usage: wgpu::BufferUsages::STORAGE,
        });
        (Some(ref_buf), Some(sa_buf))
    } else {
        (None, None)
    };

    for tile in &tiles {
        let tile_pixel_count = (tile.width * tile.height) as u64;
        let pixel_bytes = tile_pixel_count * std::mem::size_of::<PixelOut>() as u64;

        let out_buf = gpu.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tile-out"),
            size: pixel_bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });

        let mut uniforms = Uniforms::new_zero();
        uniforms.tile_origin_px = [tile.origin_x, tile.origin_y];
        uniforms.frame_size_px = [req.width, req.height];
        uniforms.tile_size_px = [tile.width, tile.height];
        uniforms.max_iter = req.max_iter;
        uniforms.tier = tier.as_u32();

        match tier {
            Tier::MidF64 => unreachable!("MidF64 doesn't go through GPU dispatch_pass"),
            Tier::Shallow => {
                uniforms.params[0] = [
                    parse_f32(ref_re),
                    parse_f32(ref_im),
                    px_dx as f32,
                    px_dy as f32,
                ];
            }
            Tier::Medium => {
                let cre = parse_f64(ref_re);
                let cim = parse_f64(ref_im);
                let (cre_hi, cre_lo) = split_ds(cre);
                let (cim_hi, cim_lo) = split_ds(cim);
                let (dx_hi, dx_lo) = split_ds(px_dx);
                let (dy_hi, dy_lo) = split_ds(px_dy);
                uniforms.params[0] = [cre_hi, cre_lo, cim_hi, cim_lo];
                uniforms.params[1] = [dx_hi, dx_lo, dy_hi, dy_lo];
            }
            Tier::DeepPerturb => {
                let half_w = req.width as f64 * 0.5;
                let half_h = req.height as f64 * 0.5;
                let origin_off_x =
                    (tile.origin_x as f64 - half_w) * px_dx - ref_offset_x;
                let origin_off_y =
                    (tile.origin_y as f64 - half_h) * px_dy - ref_offset_y;
                // f32 params (used by f32 shader if f64 pipeline isn't active)
                uniforms.params[0] = [
                    origin_off_x as f32,
                    origin_off_y as f32,
                    px_dx as f32,
                    0.0_f32,
                ];
                uniforms.params[1] = [0.0_f32, px_dy as f32, 0.0, 0.0];
                let r = reference.as_ref().expect("reference computed for deep tier");
                // SA bootstrap is currently disabled (sa_terms=0, skip_n=0).
                // Enabling it caused all-black deep frames:
                // the bootstrapped dz at skip_n was so close to the bailout
                // boundary that every pixel escaped within a couple of iters
                // with near-identical smooth values, collapsing the encoder's
                // min/max range to zero. Will revisit with BLA-style block
                // skipping which is more robust than tail-bound SA.
                uniforms.params[2] = [
                    0.0_f32,
                    r.orbit.len() as f32,
                    0.0_f32,
                    0.0,
                ];
                // f64 params (used by f64 shader if active). Same data at
                // full f64 precision so per-pixel delta_c doesn't lose bits.
                uniforms.params_f64[0] = [origin_off_x, origin_off_y];
                uniforms.params_f64[1] = [px_dx, 0.0];
                uniforms.params_f64[2] = [0.0, px_dy];
                uniforms.params_f64[3] = [0.0, 0.0];

                // DS / floatexp params: re-encode delta_c origin + per-pixel
                // steps in the f32 `params` slots the chosen deep shader reads.
                // This is where precision matters — delta_c is the tiny value
                // plain f32 can't represent at deep zoom.
                match eff_engine {
                    rosie_mandlebrot_core::config::DeepEngine::GpuDs => {
                        let (ox_hi, ox_lo) = f64_to_ds(origin_off_x);
                        let (oy_hi, oy_lo) = f64_to_ds(origin_off_y);
                        let (dx_hi, dx_lo) = f64_to_ds(px_dx);
                        let (dy_hi, dy_lo) = f64_to_ds(px_dy);
                        uniforms.params[0] = [ox_hi, ox_lo, oy_hi, oy_lo];
                        uniforms.params[1] = [dx_hi, dx_lo, dy_hi, dy_lo];
                        // params[2].y (ref_len) already set above.
                    }
                    rosie_mandlebrot_core::config::DeepEngine::GpuFe => {
                        // DS-floatexp: (hi, lo, exp) per component; exponents are
                        // bit-packed into f32 slots and bitcast back in-shader.
                        let (ore_hi, ore_lo, ore_e) = f64_to_fe(origin_off_x);
                        let (oim_hi, oim_lo, oim_e) = f64_to_fe(origin_off_y);
                        let (dx_hi, dx_lo, dx_e) = f64_to_fe(px_dx);
                        let (dy_hi, dy_lo, dy_e) = f64_to_fe(px_dy);
                        let eb = |e: i32| f32::from_bits(e as u32);
                        uniforms.params[0] = [ore_hi, ore_lo, eb(ore_e), oim_hi];
                        uniforms.params[1] = [oim_lo, eb(oim_e), dx_hi, dx_lo];
                        uniforms.params[2] = [eb(dx_e), dy_hi, dy_lo, eb(dy_e)];
                        uniforms.params[3] = [eb(r.orbit.len() as i32), 0.0, 0.0, 0.0];
                    }
                    _ => {}
                }
            }
        }

        let uni_buf = gpu.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("uniforms"),
            contents: bytemuck::bytes_of(&uniforms),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });

        // Frame-scoped buffers outlive the loop, so the bind group does not
        // have to carry them along to keep them alive.
        let bg: wgpu::BindGroup = match tier {
            Tier::MidF64 => unreachable!("MidF64 doesn't go through GPU dispatch_pass"),
            Tier::Shallow | Tier::Medium => {
                gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("bg-simple"),
                    layout: gpu.pipelines.bgl_for_tier(tier),
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: uni_buf.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 1, resource: out_buf.as_entire_binding() },
                    ],
                })
            }
            Tier::DeepPerturb => {
                let ref_buf = frame_ref_buf.as_ref().expect("deep tier built a ref buffer");
                let sa_buf = frame_sa_buf.as_ref().expect("deep tier built an SA buffer");
                gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("bg-perturb"),
                    layout: gpu.pipelines.bgl_for_tier(tier),
                    entries: &[
                        wgpu::BindGroupEntry { binding: 0, resource: uni_buf.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 1, resource: ref_buf.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 2, resource: sa_buf.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 3, resource: out_buf.as_entire_binding() },
                    ],
                })
            }
        };

        let mut enc = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("tile-encoder"),
        });

        // Timestamp the compute pass itself, so shader execution can be told
        // apart from the readback and host-side stitching that surround it.
        // Resources come from GpuCtx and are reused - allocating per dispatch
        // would make measuring cost the performance it is there to protect.
        let ts = gpu.timing.as_ref();

        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("tile-pass"),
                timestamp_writes: ts.map(|t| wgpu::ComputePassTimestampWrites {
                    query_set: &t.query_set,
                    beginning_of_pass_write_index: Some(0),
                    end_of_pass_write_index: Some(1),
                }),
            });
            // Deep tier picks its pipeline from the selected engine; other
            // tiers map straight from the tier.
            let pipeline = if tier == Tier::DeepPerturb {
                gpu.pipelines.deep_for_engine(eff_engine, req.interior_exit)
            } else {
                gpu.pipelines.for_tier(tier)
            };
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &bg, &[]);
            let gx = tile.width.div_ceil(WORKGROUP);
            let gy = tile.height.div_ceil(WORKGROUP);
            pass.dispatch_workgroups(gx, gy, 1);
        }
        if let Some(t) = ts {
            enc.resolve_query_set(&t.query_set, 0..2, &t.resolve, 0);
            enc.copy_buffer_to_buffer(&t.resolve, 0, &t.readback, 0, 16);
        }
        gpu.queue.submit(std::iter::once(enc.finish()));

        // Pixel readback first: it already polls to completion, so the
        // timestamps are resolved by the time we look at them and no additional
        // synchronisation is needed.
        let tile_pixels =
            readback::read_pixel_buffer(&gpu.device, &gpu.queue, &out_buf, tile_pixel_count, &gpu.readback_staging)?;

        if let Some(t) = ts {
            let slice = t.readback.slice(..);
            let (tx, rx) = std::sync::mpsc::channel();
            slice.map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
            let _ = gpu.device.poll(wgpu::PollType::Poll);
            if matches!(rx.recv(), Ok(Ok(()))) {
                if let Ok(data) = slice.get_mapped_range() {
                    let raw: &[u64] = bytemuck::cast_slice(&data);
                    if raw.len() >= 2 && raw[1] >= raw[0] {
                        let ns = (raw[1] - raw[0]) as f64 * f64::from(gpu.timestamp_period);
                        add_shader_ms(ns / 1.0e6);
                    }
                }
                t.readback.unmap();
            }
        }

        // Stitch the tile into the frame. At 4K there is only ever one tile
        // (tiling.rs: SINGLE_DISPATCH_PIXEL_BUDGET is 20M), so the tile buffer
        // is moved into place rather than doing 16.7 million scalar copies for
        // an identity mapping - which matters, because GPU timestamps show a
        // shallow 1080p frame is ~1% shader and ~99% host-side work like this.
        if tiles.len() == 1 && tile.width == req.width && tile.height == req.height {
            out_pixels = tile_pixels;
        } else {
            // Row-at-a-time: one memcpy per row instead of one per pixel.
            for ty in 0..tile.height as usize {
                let src = ty * tile.width as usize;
                let dst = (tile.origin_y as usize + ty) * req.width as usize
                    + tile.origin_x as usize;
                out_pixels[dst..dst + tile.width as usize]
                    .copy_from_slice(&tile_pixels[src..src + tile.width as usize]);
            }
        }
        req.bump_progress(tile_pixel_count);
        // The reference-orbit and SA buffers are frame-scoped and shared by
        // every tile, so they are released when the frame ends - dropping them
        // here would destroy them after the first tile.
    }

    add_gpu_ms(gpu_t0.elapsed().as_secs_f64() * 1000.0);
    Ok(out_pixels)
}
