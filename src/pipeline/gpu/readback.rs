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




pub(crate) struct PendingRgba32Readback {
    readback: wgpu::Buffer,
    submission: wgpu::SubmissionIndex,
    receiver: std::sync::mpsc::Receiver<Result<(), String>>,
    width: u32,
    height: u32,
    padded_bytes_per_row: u32,
}

impl PendingRgba32Readback {
    pub(crate) fn finish(self, device: &wgpu::Device) -> Result<Vec<f32>> {
        device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(self.submission),
                timeout: None,
            })
            .map_err(|error| anyhow!("GPU poll failed during pipelined export readback: {error}"))?;
        self.receiver
            .recv()
            .map_err(|_| anyhow!("GPU export readback callback was dropped"))?
            .map_err(|error| anyhow!("GPU export readback mapping failed: {error}"))?;

        let mapped = self.readback.get_mapped_range(..);
        let mut rgb = Vec::with_capacity((self.width * self.height * 3) as usize);
        for row in 0..self.height as usize {
            let row_start = row * self.padded_bytes_per_row as usize;
            for pixel in mapped[row_start..row_start + self.width as usize * 16]
                .chunks_exact(16)
            {
                rgb.push(f32::from_le_bytes(pixel[0..4].try_into().expect("RGBA32F red")));
                rgb.push(f32::from_le_bytes(pixel[4..8].try_into().expect("RGBA32F green")));
                rgb.push(f32::from_le_bytes(pixel[8..12].try_into().expect("RGBA32F blue")));
            }
        }
        drop(mapped);
        self.readback.unmap();
        if rgb.iter().any(|value| !value.is_finite()) {
            return Err(anyhow!("display-linear export readback contains NaN or infinity"));
        }
        Ok(rgb)
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn begin_rgba32_texture_region_rgb_readback(
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
) -> Result<PendingRgba32Readback> {
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
    let (sender, receiver) = std::sync::mpsc::channel();
    readback.map_async(wgpu::MapMode::Read, .., move |result| {
        let _ = sender.send(result.map_err(|error| error.to_string()));
    });
    Ok(PendingRgba32Readback {
        readback,
        submission,
        receiver,
        width,
        height,
        padded_bytes_per_row,
    })
}

const MAX_RGBA32_READBACK_CHUNK_BYTES: u64 = 64 * 1024 * 1024;

fn rgba32_readback_rows_per_chunk(width: u32) -> Result<u32> {
    if width == 0 {
        return Err(anyhow!("GPU RGBA32F readback width is zero"));
    }
    let unpadded_bytes_per_row = width
        .checked_mul(16)
        .ok_or_else(|| anyhow!("GPU RGBA32F row byte count overflows"))?;
    let padded_bytes_per_row = unpadded_bytes_per_row.div_ceil(256) * 256;
    let rows = MAX_RGBA32_READBACK_CHUNK_BYTES / u64::from(padded_bytes_per_row);
    if rows == 0 {
        return Err(anyhow!(
            "one GPU RGBA32F readback row ({padded_bytes_per_row} bytes) exceeds the chunk limit"
        ));
    }
    Ok(rows.min(u64::from(u32::MAX)) as u32)
}

#[allow(clippy::too_many_arguments)]
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

    // WebGPU adapters commonly expose a 256 MiB max_buffer_size. A large
    // full-resolution inpainting crop can legitimately exceed that even though
    // the texture itself is valid (for example, ~19.3 MP × RGBA32F is ~295 MiB).
    // Read the texture in bounded horizontal strips so no single MAP_READ buffer
    // can approach the device limit. This also makes repeated inpainting strokes
    // independent of the crop size instead of turning a large stroke into a fatal
    // wgpu validation panic.
    let rows_per_chunk = rgba32_readback_rows_per_chunk(width)?;
    let capacity = usize::try_from(width)
        .ok()
        .and_then(|w| usize::try_from(height).ok().and_then(|h| w.checked_mul(h)))
        .and_then(|pixels| pixels.checked_mul(3))
        .ok_or_else(|| anyhow!("GPU RGBA32F readback output size overflows"))?;
    let mut rgb = Vec::with_capacity(capacity);
    let mut row_offset = 0u32;

    while row_offset < height {
        let chunk_height = rows_per_chunk.min(height - row_offset);
        let (readback, padded_bytes_per_row) =
            create_rgba32_readback_buffer(device, width, chunk_height, label);
        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some(label) });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x,
                    y: y + row_offset,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bytes_per_row),
                    rows_per_image: Some(chunk_height),
                },
            },
            wgpu::Extent3d {
                width,
                height: chunk_height,
                depth_or_array_layers: 1,
            },
        );
        let submission = queue.submit(Some(encoder.finish()));
        rgb.extend(map_rgba32_readback_rgb(
            device,
            &readback,
            submission,
            width,
            chunk_height,
            padded_bytes_per_row,
        )?);
        row_offset += chunk_height;
    }

    Ok(rgb)
}

pub(super) fn read_rgba32_texture_rgb_blocking(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    width: u32,
    height: u32,
    label: &'static str,
) -> Result<Vec<f32>> {
    read_rgba32_texture_region_rgb_blocking(
        device, queue, texture, 0, 0, width, height, width, height, label,
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

#[cfg(test)]
mod tests {
    use super::{rgba32_readback_rows_per_chunk, MAX_RGBA32_READBACK_CHUNK_BYTES};

    #[test]
    fn rgba32_readback_chunks_stay_below_the_safe_buffer_budget() {
        let width = 8_256u32;
        let rows = rgba32_readback_rows_per_chunk(width).unwrap();
        let padded = (width * 16).div_ceil(256) * 256;
        assert!(u64::from(rows) * u64::from(padded) <= MAX_RGBA32_READBACK_CHUNK_BYTES);
        assert!(rows > 0);
    }

    #[test]
    fn rgba32_readback_rejects_zero_width() {
        assert!(rgba32_readback_rows_per_chunk(0).is_err());
    }
}
