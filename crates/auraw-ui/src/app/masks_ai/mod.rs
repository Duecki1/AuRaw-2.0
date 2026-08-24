use super::*;

const AI_MASK_SOURCE_MAX_EDGE: u32 = 4096;
const AI_MASK_SOURCE_MAX_PIXELS: u64 = 12_000_000;

pub(super) fn ai_mask_source_proxy_edge(width: u32, height: u32) -> u32 {
    let longest = width.max(height).max(1);
    let shortest = width.min(height).max(1);
    let pixel_limited_edge = ((AI_MASK_SOURCE_MAX_PIXELS as f64 * longest as f64 / shortest as f64)
        .sqrt()
        .floor() as u32)
        .max(1);
    longest.min(AI_MASK_SOURCE_MAX_EDGE).min(pixel_limited_edge)
}

mod dialogs;
mod object;
mod source;
mod state;
mod subject;

#[cfg(test)]
mod tests;
