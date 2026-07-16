use crate::pipeline::LoadedRaw;
use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

const MAX_EVENTS: usize = 300;
const FINGERPRINT_SAMPLES: usize = 4096;

struct DiagnosticState {
    started: Instant,
    device_info: Option<String>,
    gpu_info: Option<String>,
    events: VecDeque<String>,
}

impl DiagnosticState {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            device_info: None,
            gpu_info: None,
            events: VecDeque::with_capacity(MAX_EVENTS),
        }
    }

    fn push(&mut self, message: &str) {
        let elapsed = self.started.elapsed();
        let line = format!(
            "[+{:>5}.{:03}s] {}",
            elapsed.as_secs(),
            elapsed.subsec_millis(),
            message
        );
        if self.events.len() == MAX_EVENTS {
            self.events.pop_front();
        }
        self.events.push_back(line);
    }
}

fn state() -> &'static Mutex<DiagnosticState> {
    static STATE: OnceLock<Mutex<DiagnosticState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(DiagnosticState::new()))
}

pub fn record(message: impl AsRef<str>) {
    let message = message.as_ref();
    if let Ok(mut state) = state().lock() {
        state.push(message);
    }
    log::info!("diagnostics: {message}");
}

pub fn set_device_info(info: String) {
    if let Ok(mut state) = state().lock() {
        state.device_info = Some(info);
        state.push("Android device information captured");
    }
}

pub fn set_gpu_info(info: String) {
    if let Ok(mut state) = state().lock() {
        state.gpu_info = Some(info);
        state.push("GPU adapter information captured");
    }
}

pub fn clear() {
    if let Ok(mut state) = state().lock() {
        state.events.clear();
        state.started = Instant::now();
        state.push("Diagnostic event log cleared");
    }
}

pub fn snapshot() -> String {
    let Ok(state) = state().lock() else {
        return "AuRaw diagnostics are temporarily unavailable because the log is locked."
            .to_owned();
    };

    let mut output = String::with_capacity(12 * 1024);
    let _ = writeln!(output, "AuRaw {}", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(output, "revision={}", crate::SOURCE_REVISION);
    let _ = writeln!(
        output,
        "target_os={} target_arch={} pointer_width={}bit",
        std::env::consts::OS,
        std::env::consts::ARCH,
        usize::BITS
    );

    if let Some(device) = &state.device_info {
        output.push_str("\n[Android device]\n");
        output.push_str(device.trim());
        output.push('\n');
    }

    if let Some(gpu) = &state.gpu_info {
        output.push_str("\n[GPU adapter]\n");
        output.push_str(gpu.trim());
        output.push('\n');
    }

    output.push_str("\n[Events]\n");
    if state.events.is_empty() {
        output.push_str("No diagnostic events yet. Open a RAW or start an export.\n");
    } else {
        for event in &state.events {
            output.push_str(event);
            output.push('\n');
        }
    }
    output
}

pub fn record_raw(label: &str, raw: &LoadedRaw) {
    let raw_signature = sampled_u16_fingerprint(&raw.raw_pixels);
    let cfa_signature = sampled_u8_fingerprint(&raw.color_indices);
    let black_signature = sampled_f32_fingerprint(&raw.black_levels_per_pixel);
    let cfa_sample_counts = sampled_cfa_counts(&raw.color_indices);

    record(format!(
        "{label}: camera=\"{} {}\" size={}x{} cfa={:?} raw_len={} cfa_len={} black_map_len={}",
        raw.camera_make,
        raw.camera_model,
        raw.width,
        raw.height,
        raw.cfa_kind,
        raw.raw_pixels.len(),
        raw.color_indices.len(),
        raw.black_levels_per_pixel.len(),
    ));
    record(format!(
        "{label}: wb={:?} black={:?} white={:?}",
        raw.wb_coeffs, raw.black_levels, raw.white_levels
    ));
    record(format!(
        "{label}: cam_to_srgb=[{:?}, {:?}, {:?}]",
        raw.cam_to_srgb[0], raw.cam_to_srgb[1], raw.cam_to_srgb[2]
    ));
    record(format!(
        "{label}: sampled_fingerprint raw={raw_signature:016x} cfa={cfa_signature:016x} black={black_signature:016x} sampled_cfa_counts={cfa_sample_counts:?}"
    ));
}

fn sample_indices(length: usize) -> impl Iterator<Item = usize> {
    let count = length.min(FINGERPRINT_SAMPLES);
    (0..count).map(move |sample| {
        if count <= 1 {
            0
        } else {
            sample.saturating_mul(length.saturating_sub(1)) / (count - 1)
        }
    })
}

fn fnv1a_step(hash: u64, byte: u8) -> u64 {
    (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
}

fn sampled_u16_fingerprint(values: &[u16]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for index in sample_indices(values.len()) {
        for byte in values[index].to_le_bytes() {
            hash = fnv1a_step(hash, byte);
        }
    }
    hash
}

fn sampled_u8_fingerprint(values: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for index in sample_indices(values.len()) {
        hash = fnv1a_step(hash, values[index]);
    }
    hash
}

fn sampled_f32_fingerprint(values: &[f32]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for index in sample_indices(values.len()) {
        for byte in values[index].to_bits().to_le_bytes() {
            hash = fnv1a_step(hash, byte);
        }
    }
    hash
}

fn sampled_cfa_counts(values: &[u8]) -> [usize; 4] {
    let mut counts = [0usize; 4];
    for index in sample_indices(values.len()) {
        if let Some(count) = counts.get_mut(usize::from(values[index])) {
            *count += 1;
        }
    }
    counts
}
