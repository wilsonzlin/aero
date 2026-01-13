#![allow(dead_code)]

use anyhow::Result;

/// Backwards-compatible shim for older integration tests.
///
/// Most tests should use `common::wgpu::*` directly. This module exists so tests can `mod
/// wgpu_common;` without duplicating boilerplate device/queue setup logic.
pub async fn create_device_queue(
    device_label: &str,
) -> Result<(wgpu::Device, wgpu::Queue, bool)> {
    crate::common::wgpu::create_device_queue(device_label).await
}

pub async fn read_texture_rgba8(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
) -> Result<Vec<u8>> {
    crate::common::wgpu::read_texture_rgba8(device, queue, texture, width, height).await
}
