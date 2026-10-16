//! The WGSL kernels, compiled into the binary so it runs from any directory.
//!
//! `MANDELBROT_SHADER_DIR` overrides them with files on disk, which is how a
//! shader change is A/B tested without a rebuild.

use rosie_mandlebrot_core::error::{MbError, MbResult};
use std::borrow::Cow;
use std::path::Path;

/// Every kernel by file name, in the order the pipelines are built.
pub const EMBEDDED: &[(&str, &str)] = &[
    ("mandelbrot_f32.wgsl", include_str!("../../../../shaders/mandelbrot_f32.wgsl")),
    ("mandelbrot_ds.wgsl", include_str!("../../../../shaders/mandelbrot_ds.wgsl")),
    ("mandelbrot_perturb.wgsl", include_str!("../../../../shaders/mandelbrot_perturb.wgsl")),
    ("mandelbrot_perturb_ds.wgsl", include_str!("../../../../shaders/mandelbrot_perturb_ds.wgsl")),
    ("mandelbrot_perturb_fe.wgsl", include_str!("../../../../shaders/mandelbrot_perturb_fe.wgsl")),
    ("mandelbrot_perturb_f64.wgsl", include_str!("../../../../shaders/mandelbrot_perturb_f64.wgsl")),
];

/// The source of one kernel: from `dir` when given, otherwise the embedded copy.
pub fn source(dir: Option<&Path>, name: &str) -> MbResult<Cow<'static, str>> {
    if let Some(dir) = dir {
        let path = dir.join(name);
        return std::fs::read_to_string(&path).map(Cow::Owned).map_err(|e| MbError::io(&path, e));
    }
    EMBEDDED
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, src)| Cow::Borrowed(*src))
        .ok_or_else(|| MbError::GpuInit(format!("no embedded shader named {name}")))
}

#[cfg(test)]
mod tests {
    use super::EMBEDDED;
    use rosie_mandlebrot_core::types::{PixelOut, RefOrbitEntry, RefOrbitEntryDs, RefOrbitEntryF64, Uniforms};
    use naga::valid::{Capabilities, ValidationFlags, Validator};
    use std::mem::{offset_of, size_of};

    fn parse(name: &str, src: &str) -> naga::Module {
        naga::front::wgsl::parse_str(src).unwrap_or_else(|e| panic!("{name}: {}", e.emit_to_string(src)))
    }

    fn caps(name: &str) -> Capabilities {
        if name.contains("f64") { Capabilities::FLOAT64 } else { Capabilities::empty() }
    }

    #[test]
    fn every_shader_parses_and_validates() {
        for (name, src) in EMBEDDED {
            let module = parse(name, src);
            Validator::new(ValidationFlags::all(), caps(name))
                .validate(&module)
                .unwrap_or_else(|e| panic!("{name}: {}", e.emit_to_string(src)));
        }
    }

    #[test]
    fn every_shader_file_is_embedded() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../shaders");
        let mut on_disk: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|n| n.ends_with(".wgsl"))
            .collect();
        on_disk.sort();
        let mut embedded: Vec<String> = EMBEDDED.iter().map(|(n, _)| n.to_string()).collect();
        embedded.sort();
        assert_eq!(on_disk, embedded);
    }

    /// (member name, offset, size) of a WGSL struct, laid out by naga.
    fn wgsl_struct(module: &naga::Module, name: &str) -> Option<(u32, Vec<(String, u32)>)> {
        let mut layouter = naga::proc::Layouter::default();
        layouter.update(module.to_ctx()).unwrap();
        module.types.iter().find_map(|(handle, ty)| match &ty.inner {
            naga::TypeInner::Struct { members, span } if ty.name.as_deref() == Some(name) => {
                let _ = handle;
                Some((*span, members.iter().map(|m| (m.name.clone().unwrap_or_default(), m.offset)).collect()))
            }
            _ => None,
        })
    }

    fn rust_offset(strukt: &str, field: &str) -> usize {
        match (strukt, field) {
            ("Uniforms", "tile_origin_px") => offset_of!(Uniforms, tile_origin_px),
            ("Uniforms", "frame_size_px") => offset_of!(Uniforms, frame_size_px),
            ("Uniforms", "tile_size_px") => offset_of!(Uniforms, tile_size_px),
            ("Uniforms", "max_iter") => offset_of!(Uniforms, max_iter),
            ("Uniforms", "tier") => offset_of!(Uniforms, tier),
            ("Uniforms", "params") => offset_of!(Uniforms, params),
            ("Uniforms", "params_f64") => offset_of!(Uniforms, params_f64),
            ("PixelOut", "iter") => offset_of!(PixelOut, iter),
            ("PixelOut", "smooth_val") => offset_of!(PixelOut, smooth_val),
            ("RefOrbitEntry", "z_re") => offset_of!(RefOrbitEntry, z_re),
            ("RefOrbitEntry", "z_im") => offset_of!(RefOrbitEntry, z_im),
            ("RefOrbitEntry", "z_mag_sq") => offset_of!(RefOrbitEntry, z_mag_sq),
            ("RefOrbitEntry", "pad") => offset_of!(RefOrbitEntry, _pad),
            ("RefOrbitEntryF64", "z_re") => offset_of!(RefOrbitEntryF64, z_re),
            ("RefOrbitEntryF64", "z_im") => offset_of!(RefOrbitEntryF64, z_im),
            ("RefOrbitEntryF64", "z_mag_sq") => offset_of!(RefOrbitEntryF64, z_mag_sq),
            ("RefOrbitEntryF64", "pad") => offset_of!(RefOrbitEntryF64, _pad),
            _ => panic!("{strukt}.{field} exists in WGSL but not in the Rust struct"),
        }
    }

    /// Every WGSL struct that mirrors a Rust POD must agree with it member by
    /// member. `Uniforms` may be a prefix (most kernels omit `params_f64`), but
    /// the GPU reads the Rust bytes, so every member it does declare must sit
    /// at the Rust offset.
    #[test]
    fn wgsl_structs_match_rust_layouts() {
        let rust_size = |s: &str| match s {
            "Uniforms" => size_of::<Uniforms>(),
            "PixelOut" => size_of::<PixelOut>(),
            "RefOrbitEntry" => size_of::<RefOrbitEntry>(),
            "RefOrbitEntryF64" => size_of::<RefOrbitEntryF64>(),
            _ => unreachable!(),
        };
        let mut checked = 0;
        for (name, src) in EMBEDDED {
            let module = parse(name, src);
            for strukt in ["Uniforms", "PixelOut", "RefOrbitEntry", "RefOrbitEntryF64"] {
                let Some((span, members)) = wgsl_struct(&module, strukt) else { continue };
                for (field, off) in &members {
                    assert_eq!(*off as usize, rust_offset(strukt, field), "{name}: {strukt}.{field} offset");
                }
                let full = rust_size(strukt);
                if strukt == "Uniforms" {
                    assert!(span as usize <= full, "{name}: WGSL Uniforms ({span}) larger than Rust ({full})");
                } else {
                    assert_eq!(span as usize, full, "{name}: {strukt} size");
                }
                checked += 1;
            }
        }
        // Six kernels each declare Uniforms and PixelOut, two declare an orbit entry.
        assert_eq!(checked, 14);
    }

    /// The DS and floatexp kernels bind the orbit as `array<vec4<f32>>`: one
    /// 16-byte element per `RefOrbitEntryDs`.
    #[test]
    fn ds_orbit_entry_is_one_vec4() {
        assert_eq!(size_of::<RefOrbitEntryDs>(), 16);
        assert_eq!(size_of::<PixelOut>(), 8);
    }
}
