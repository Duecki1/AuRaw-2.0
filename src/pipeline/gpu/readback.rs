use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn read_rgba8_texture_region_blocking(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    texture_width: u32,
    texture_height: u32,
    label: &'static str,
) -> Result<Vec<u8>> {
    let right = x
        .checked_add(width)
        .ok_or_else(|| anyhow!("GPU readback rectangle overflows horizontally"))?;
    let bottom = y
        .checked_add(height)
        .ok_or_else(|| anyhow!("GPU readback rectangle overflows vertically"))?;
    if width == 0 || height == 0 || right > texture_width || bottom > texture_height {
        return Err(anyhow!("invalid GPU RGBA8 readback rectangle"));
    }

    let unpadded_bytes_per_row = width * 4;
    let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(256) * 256;
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: u64::from(padded_bytes_per_row) * u64::from(height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut encoder =
        device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some(label) });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d { x, y, z: 0 },
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bytes_per_row),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    let submission = queue.submit(Some(encoder.finish()));
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    readback.map_async(wgpu::MapMode::Read, .., move |result| {
        let _ = sender.send(result);
    });
    device
        .poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: None,
        })
        .map_err(|error| anyhow!("GPU poll failed during thumbnail readback: {error}"))?;
    receiver
        .recv()
        .map_err(|_| anyhow!("GPU thumbnail readback callback was dropped"))?
        .map_err(|error| anyhow!("GPU thumbnail readback mapping failed: {error}"))?;

    let mapped = readback.get_mapped_range(..);
    let mut rgba = vec![0u8; (width * height * 4) as usize];
    for row in 0..height as usize {
        let source = row * padded_bytes_per_row as usize;
        let destination = row * unpadded_bytes_per_row as usize;
        rgba[destination..destination + unpadded_bytes_per_row as usize]
            .copy_from_slice(&mapped[source..source + unpadded_bytes_per_row as usize]);
    }
    drop(mapped);
    readback.unmap();
    Ok(rgba)
}

pub(super) fn read_rgba32_texture_region_rgb_blocking(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    texture_width: u32,
    texture_height: u32,
    label: &'static str,
) -> Result<Vec<f32>> {
    let right = x
        .checked_add(width)
        .ok_or_else(|| anyhow!("GPU readback rectangle overflows horizontally"))?;
    let bottom = y
        .checked_add(height)
        .ok_or_else(|| anyhow!("GPU readback rectangle overflows vertically"))?;
    if width == 0 || height == 0 || right > texture_width || bottom > texture_height {
        return Err(anyhow!("invalid GPU RGBA32F readback rectangle"));
    }

    let (readback, padded_bytes_per_row) =
        create_rgba32_readback_buffer(device, width, height, label);
    let mut encoder =
        device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some(label) });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d { x, y, z: 0 },
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bytes_per_row),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    let submission = queue.submit(Some(encoder.finish()));
    map_rgba32_readback_rgb(
        device,
        &readback,
        submission,
        width,
        height,
        padded_bytes_per_row,
    )
}

pub(super) fn read_rgba32_texture_rgb_blocking(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
    label: &'static str,
) -> Result<Vec<f32>> {
    let (readback, padded_bytes_per_row) =
        create_rgba32_readback_buffer(device, width, height, label);
    let mut encoder =
        device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some(label) });
    encode_rgba32_texture_copy(
        &mut encoder,
        texture,
        &readback,
        width,
        height,
        padded_bytes_per_row,
    );
    let submission = queue.submit(Some(encoder.finish()));
    map_rgba32_readback_rgb(
        device,
        &readback,
        submission,
        width,
        height,
        padded_bytes_per_row,
    )
}

pub(super) fn create_rgba32_readback_buffer(
    device: &wgpu::Device,
    width: u32,
    height: u32,
    label: &'static str,
) -> (wgpu::Buffer, u32) {
    let unpadded_bytes_per_row = width * 16;
    let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(256) * 256;
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: u64::from(padded_bytes_per_row) * u64::from(height),
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    (buffer, padded_bytes_per_row)
}

pub(super) fn encode_rgba32_texture_copy(
    encoder: &mut wgpu::CommandEncoder,
    texture: &wgpu::Texture,
    readback: &wgpu::Buffer,
    width: u32,
    height: u32,
    padded_bytes_per_row: u32,
) {
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: readback,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bytes_per_row),
                rows_per_image: Some(height),
            },
        },
        texture_size(width, height),
    );
}

pub(super) fn map_rgba32_readback_rgb(
    device: &wgpu::Device,
    readback: &wgpu::Buffer,
    submission: wgpu::SubmissionIndex,
    width: u32,
    height: u32,
    padded_bytes_per_row: u32,
) -> Result<Vec<f32>> {
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    readback.map_async(wgpu::MapMode::Read, .., move |result| {
        let _ = sender.send(result);
    });
    device
        .poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: None,
        })
        .map_err(|error| anyhow!("GPU poll failed during scene readback: {error}"))?;
    receiver
        .recv()
        .map_err(|_| anyhow!("GPU scene readback callback was dropped"))?
        .map_err(|error| anyhow!("GPU scene readback mapping failed: {error}"))?;

    let mapped = readback.get_mapped_range(..);
    let mut rgb = Vec::with_capacity((width * height * 3) as usize);
    for row in 0..height as usize {
        let row_start = row * padded_bytes_per_row as usize;
        for column in 0..width as usize {
            let pixel_start = row_start + column * 16;
            for channel in 0..3 {
                let offset = pixel_start + channel * 4;
                let bytes = mapped
                    .get(offset..offset + 4)
                    .ok_or_else(|| anyhow!("GPU RGBA32F readback buffer is truncated"))?;
                let bytes = <[u8; 4]>::try_from(bytes)
                    .map_err(|_| anyhow!("GPU RGBA32F readback channel has an invalid width"))?;
                rgb.push(f32::from_le_bytes(bytes));
            }
        }
    }
    drop(mapped);
    readback.unmap();
    if rgb.iter().any(|value| !value.is_finite()) {
        return Err(anyhow!("scene texture readback contains NaN or infinity"));
    }
    Ok(rgb)
}
