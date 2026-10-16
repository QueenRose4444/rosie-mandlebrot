//! Helpers for downloading a storage buffer back to CPU as Vec<PixelOut>.

use super::buffers::PixelOut;
use rosie_mandlebrot_core::error::{MbError, MbResult};
use std::sync::mpsc;

pub fn read_pixel_buffer(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    src: &wgpu::Buffer,
    pixel_count: u64,
    cache: &std::sync::Mutex<Option<wgpu::Buffer>>,
) -> MbResult<Vec<PixelOut>> {
    let byte_len = pixel_count * std::mem::size_of::<PixelOut>() as u64;

    // Reuse the staging buffer across frames, growing it only when a larger
    // frame arrives. At 4K this is 134 MB that would otherwise be allocated and
    // dropped every frame.
    let mut guard = cache.lock().expect("readback cache poisoned");
    let needs_new = match guard.as_ref() {
        Some(b) => b.size() < byte_len,
        None => true,
    };
    if needs_new {
        *guard = Some(device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback-staging"),
            size: byte_len,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
    }
    let staging = guard.as_ref().expect("just ensured");

    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("readback-copy"),
    });
    enc.copy_buffer_to_buffer(src, 0, staging, 0, byte_len);
    queue.submit(std::iter::once(enc.finish()));

    let slice = staging.slice(0..byte_len);
    let (tx, rx) = mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device
        .poll(wgpu::PollType::Wait { submission_index: None, timeout: None })
        .map_err(|e| MbError::GpuDispatch(format!("device poll: {e}")))?;
    rx.recv()
        .map_err(|e| MbError::GpuDispatch(format!("readback channel: {e}")))?
        .map_err(|e| MbError::GpuDispatch(format!("buffer map_async: {e:?}")))?;

    let data = slice
        .get_mapped_range()
        .map_err(|e| MbError::GpuDispatch(format!("get_mapped_range: {e:?}")))?;
    let pixels: Vec<PixelOut> = bytemuck::cast_slice::<u8, PixelOut>(&data).to_vec();
    drop(data);
    staging.unmap();
    Ok(pixels)
}

/// Download the first `byte_len` bytes of `src` through a reused staging buffer.
pub fn read_bytes(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    src: &wgpu::Buffer,
    byte_len: u64,
    cache: &std::sync::Mutex<Option<wgpu::Buffer>>,
) -> MbResult<Vec<u8>> {
    let mut guard = cache.lock().expect("readback cache poisoned");
    if guard.as_ref().is_none_or(|b| b.size() < byte_len) {
        *guard = Some(device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback-staging-bytes"),
            size: byte_len,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        }));
    }
    let staging = guard.as_ref().expect("just ensured");
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("readback-copy") });
    enc.copy_buffer_to_buffer(src, 0, staging, 0, byte_len);
    queue.submit(std::iter::once(enc.finish()));
    let slice = staging.slice(0..byte_len);
    let (tx, rx) = mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device
        .poll(wgpu::PollType::Wait { submission_index: None, timeout: None })
        .map_err(|e| MbError::GpuDispatch(format!("device poll: {e}")))?;
    rx.recv()
        .map_err(|e| MbError::GpuDispatch(format!("readback channel: {e}")))?
        .map_err(|e| MbError::GpuDispatch(format!("buffer map_async: {e:?}")))?;
    let data = slice
        .get_mapped_range()
        .map_err(|e| MbError::GpuDispatch(format!("get_mapped_range: {e:?}")))?;
    let out = data.to_vec();
    drop(data);
    staging.unmap();
    Ok(out)
}

/// Nanoseconds between the two timestamps of the last timed pass, once its
/// work has completed (call after a blocking readback of that work).
pub fn read_timestamp_ns(gpu: &super::GpuCtx, t: &super::GpuTiming) -> Option<f64> {
    let slice = t.readback.slice(..);
    let (tx, rx) = mpsc::channel();
    slice.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    let _ = gpu.device.poll(wgpu::PollType::Poll);
    let mut ns = None;
    if matches!(rx.recv(), Ok(Ok(()))) {
        if let Ok(data) = slice.get_mapped_range() {
            let raw: &[u64] = bytemuck::cast_slice(&data);
            if raw.len() >= 2 && raw[1] >= raw[0] {
                ns = Some((raw[1] - raw[0]) as f64 * f64::from(gpu.timestamp_period));
            }
        }
        t.readback.unmap();
    }
    ns
}
