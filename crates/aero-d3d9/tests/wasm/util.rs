#![cfg(target_arch = "wasm32")]

use aero_d3d9::resources::ResourceManager;
use aero_d3d9::resources::ResourceManagerOptions;

use futures::channel::oneshot;

pub async fn init_manager() -> ResourceManager {
    init_manager_with_options(ResourceManagerOptions::default()).await
}

pub async fn init_manager_with_options(options: ResourceManagerOptions) -> ResourceManager {
    let instance = wgpu::Instance::default();

    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        })
        .await
        .expect("request_adapter");

    let supported = adapter.features();

    // Opt into texture compression features when available so we exercise both code paths across
    // platforms (WebGPU vs WebGL2 fallback). CI can force deterministic CPU fallback via
    // `AERO_DISABLE_WGPU_TEXTURE_COMPRESSION=1`.
    let texture_compression_disabled = std::env::var("AERO_DISABLE_WGPU_TEXTURE_COMPRESSION")
        .ok()
        .map(|raw| {
            let v = raw.trim();
            v == "1"
                || v.eq_ignore_ascii_case("true")
                || v.eq_ignore_ascii_case("yes")
                || v.eq_ignore_ascii_case("on")
        })
        .unwrap_or(false);

    let mut features = wgpu::Features::empty();
    if !texture_compression_disabled {
        for feature in [
            wgpu::Features::TEXTURE_COMPRESSION_BC,
            wgpu::Features::TEXTURE_COMPRESSION_ETC2,
            wgpu::Features::TEXTURE_COMPRESSION_ASTC_HDR,
        ] {
            if supported.contains(feature) {
                features |= feature;
            }
        }
    }
    if supported.contains(wgpu::Features::ADDRESS_MODE_CLAMP_TO_BORDER) {
        features |= wgpu::Features::ADDRESS_MODE_CLAMP_TO_BORDER;
    }

    let limits = wgpu::Limits::downlevel_webgl2_defaults();

    let (device, queue) = adapter
        .request_device(
            &wgpu::DeviceDescriptor {
                label: Some("aero-d3d9 wasm tests"),
                required_features: features,
                required_limits: limits,
            },
            None,
        )
        .await
        .expect("request_device");

    ResourceManager::new(device, queue, options)
}

pub fn rgb_to_565(rgb: [u8; 3]) -> u16 {
    let r = (rgb[0] as u16 * 31 + 127) / 255;
    let g = (rgb[1] as u16 * 63 + 127) / 255;
    let b = (rgb[2] as u16 * 31 + 127) / 255;
    (r << 11) | (g << 5) | b
}

pub fn bc1_solid_block(rgb: [u8; 3]) -> [u8; 8] {
    let c = rgb_to_565(rgb);
    let mut out = [0u8; 8];
    out[0..2].copy_from_slice(&c.to_le_bytes());
    out[2..4].copy_from_slice(&c.to_le_bytes());
    // indices = 0 => use color0
    out
}

pub fn bc2_solid_block(rgb: [u8; 3]) -> [u8; 16] {
    let mut out = [0u8; 16];
    // Alpha: 0xF for all pixels
    out[0..8].copy_from_slice(&0xFFFF_FFFF_FFFF_FFFFu64.to_le_bytes());
    let c = rgb_to_565(rgb);
    out[8..10].copy_from_slice(&c.to_le_bytes());
    out[10..12].copy_from_slice(&c.to_le_bytes());
    // indices = 0
    out
}

pub fn bc3_solid_block(rgb: [u8; 3]) -> [u8; 16] {
    let mut out = [0u8; 16];
    // Alpha endpoints, then indices (all 0 => a0).
    out[0] = 0xFF;
    out[1] = 0xFF;
    // bytes 2..8 are already 0.
    let c = rgb_to_565(rgb);
    out[8..10].copy_from_slice(&c.to_le_bytes());
    out[10..12].copy_from_slice(&c.to_le_bytes());
    // indices = 0
    out
}

pub fn align_bytes_per_row(bytes_per_row: u32) -> u32 {
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    ((bytes_per_row + align - 1) / align) * align
}

pub async fn read_texture_rgba8(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
) -> Vec<u8> {
    let bytes_per_row = width * 4;
    let padded_bpr = align_bytes_per_row(bytes_per_row);
    let buffer_size = padded_bpr as u64 * height as u64;

    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("readback"),
        size: buffer_size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    encoder.copy_texture_to_buffer(
        wgpu::ImageCopyTexture {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::ImageCopyBuffer {
            buffer: &buffer,
            layout: wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(padded_bpr),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    queue.submit([encoder.finish()]);

    let slice = buffer.slice(..);
    let (tx, rx) = oneshot::channel();
    slice.map_async(wgpu::MapMode::Read, move |res| {
        let _ = tx.send(res);
    });

    device.poll(wgpu::Maintain::Poll);
    rx.await.expect("map callback").expect("map_async");

    let mapped = slice.get_mapped_range();
    let mut out = vec![0u8; (bytes_per_row * height) as usize];
    for y in 0..height as usize {
        let src_off = y * padded_bpr as usize;
        let dst_off = y * bytes_per_row as usize;
        out[dst_off..dst_off + bytes_per_row as usize]
            .copy_from_slice(&mapped[src_off..src_off + bytes_per_row as usize]);
    }
    drop(mapped);
    buffer.unmap();
    out
}
