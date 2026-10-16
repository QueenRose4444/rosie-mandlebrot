//! Splits a frame into 512x512 tiles. Edge tiles may be smaller (the actual
//! tile size is passed in uniforms; the shader bounds-checks against it).

use crate::types::TILE_SIZE;

#[derive(Debug, Clone, Copy)]
pub struct Tile {
    pub origin_x: u32,
    pub origin_y: u32,
    pub width: u32,
    pub height: u32,
}

/// Number of pixels above which we tile a frame instead of issuing it as one
/// dispatch. Below this we run the whole frame in one dispatch, which has far
/// less CPU overhead (single buffer alloc, single bind group, single readback).
///
/// Picked so that 4K (16.8M pixels) still goes as one dispatch: a 16.8M ×
/// 8-byte PixelOut output buffer is 134 MiB, fine for any modern GPU. 8K
/// (67M pixels, 537 MiB) is borderline and will tile to keep VRAM bounded.
const SINGLE_DISPATCH_PIXEL_BUDGET: u64 = 20_000_000;

pub fn tiles_for(frame_w: u32, frame_h: u32) -> Vec<Tile> {
    let total = frame_w as u64 * frame_h as u64;
    if total <= SINGLE_DISPATCH_PIXEL_BUDGET {
        return vec![Tile {
            origin_x: 0,
            origin_y: 0,
            width: frame_w,
            height: frame_h,
        }];
    }

    let mut out = Vec::new();
    let mut y = 0u32;
    while y < frame_h {
        let h = TILE_SIZE.min(frame_h - y);
        let mut x = 0u32;
        while x < frame_w {
            let w = TILE_SIZE.min(frame_w - x);
            out.push(Tile { origin_x: x, origin_y: y, width: w, height: h });
            x += TILE_SIZE;
        }
        y += TILE_SIZE;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_frame_is_single_dispatch() {
        let tiles = tiles_for(4096, 4096);
        assert_eq!(tiles.len(), 1);
        assert_eq!(tiles[0].width, 4096);
        assert_eq!(tiles[0].height, 4096);
    }

    #[test]
    fn huge_frame_tiles() {
        // 8K (8192x8192 = 67M pixels) exceeds the single-dispatch budget.
        let tiles = tiles_for(8192, 8192);
        assert!(tiles.len() > 1);
        let max_x = tiles.iter().map(|t| t.origin_x + t.width).max().unwrap();
        let max_y = tiles.iter().map(|t| t.origin_y + t.height).max().unwrap();
        assert_eq!(max_x, 8192);
        assert_eq!(max_y, 8192);
    }
}
