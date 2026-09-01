use anyhow::{Context, Result};
use ort::{
    ep::{ExecutionProvider, ExecutionProviderDispatch},
    session::{
        builder::{GraphOptimizationLevel, SessionBuilder},
        Session,
    },
};
#[cfg(not(target_os = "android"))]
use std::sync::atomic::{AtomicBool, Ordering};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

#[cfg(not(target_os = "android"))]
static AI_ACCELERATION_ENABLED: AtomicBool = AtomicBool::new(true);
#[cfg(not(target_os = "android"))]
static AI_GPU_MEMORY_QUARANTINED: AtomicBool = AtomicBool::new(false);
#[cfg(not(target_os = "android"))]
static AI_GPU_MEMORY_FAILURE: AtomicBool = AtomicBool::new(false);

#[cfg(not(target_os = "android"))]
pub fn set_ai_acceleration_enabled(enabled: bool) {
    if enabled {
        AI_GPU_MEMORY_QUARANTINED.store(false, Ordering::Release);
        AI_GPU_MEMORY_FAILURE.store(false, Ordering::Release);
    }
    if AI_ACCELERATION_ENABLED.swap(enabled, Ordering::AcqRel) != enabled {
        if let Ok(mut statuses) = provider_statuses().lock() {
            statuses.clear();
        }
        calibraw_core::diagnostics::record(format!(
            "AI GPU acceleration {} in Settings",
            if enabled { "enabled" } else { "disabled" }
        ));
        crate::model_runtime::invalidate_for_provider_change();
    }
}

pub fn ai_acceleration_enabled() -> bool {
    #[cfg(not(target_os = "android"))]
    {
        AI_ACCELERATION_ENABLED.load(Ordering::Acquire)
            && !AI_GPU_MEMORY_QUARANTINED.load(Ordering::Acquire)
    }
    #[cfg(target_os = "android")]
    {
        true
    }
}

#[cfg(not(target_os = "android"))]
pub fn take_ai_gpu_memory_failure() -> bool {
    AI_GPU_MEMORY_FAILURE.swap(false, Ordering::AcqRel)
}

#[cfg(target_os = "android")]
pub fn take_ai_gpu_memory_failure() -> bool {
    false
}

#[cfg(not(target_os = "android"))]
fn quarantine_ai_gpu_after_memory_failure(operation: &str, provider: &str, error: &anyhow::Error) {
    AI_GPU_MEMORY_QUARANTINED.store(true, Ordering::Release);
    AI_GPU_MEMORY_FAILURE.store(true, Ordering::Release);
    let message = format!(
        "AI {operation} exhausted {provider} GPU memory; cancelled the AI job and disabled GPU AI until it is explicitly re-enabled"
    );
    log::error!("{message}: {error:#}");
    calibraw_core::diagnostics::record(message);
}

#[cfg(target_os = "android")]
fn quarantine_ai_gpu_after_memory_failure(
    _operation: &str,
    _provider: &str,
    _error: &anyhow::Error,
) {
}

fn is_gpu_memory_failure(error: &anyhow::Error) -> bool {
    let message = format!("{error:#}").to_ascii_lowercase();
    (message.contains("out of memory") || message.contains("failed to allocate memory"))
        && (message.contains("cuda")
            || message.contains("directml")
            || message.contains("tensorrt")
            || message.contains("rocm")
            || message.contains("gpu")
            || message.contains("bfc_arena"))
}

#[derive(Clone)]
pub enum ModelSource {
    Path(PathBuf),
    Bytes(Arc<[u8]>),
}

impl From<&Path> for ModelSource {
    fn from(value: &Path) -> Self {
        Self::Path(value.to_path_buf())
    }
}

impl From<&PathBuf> for ModelSource {
    fn from(value: &PathBuf) -> Self {
        Self::Path(value.clone())
    }
}

impl From<PathBuf> for ModelSource {
    fn from(value: PathBuf) -> Self {
        Self::Path(value)
    }
}

impl From<&[u8]> for ModelSource {
    fn from(value: &[u8]) -> Self {
        Self::Bytes(Arc::from(value))
    }
}

impl From<Vec<u8>> for ModelSource {
    fn from(value: Vec<u8>) -> Self {
        Self::Bytes(value.into())
    }
}

impl From<Arc<[u8]>> for ModelSource {
    fn from(value: Arc<[u8]>) -> Self {
        Self::Bytes(value)
    }
}

impl ModelSource {
    fn description(&self) -> String {
        match self {
            Self::Path(path) => path.display().to_string(),
            Self::Bytes(bytes) => format!("in-memory ONNX model ({} bytes)", bytes.len()),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CpuFallbackProfile {
    #[default]
    Default,
    WindowsSamEncoder,
}

#[derive(Clone, Debug)]
pub struct SessionOptions {
    pub model_name: &'static str,
    pub allow_acceleration: bool,
    pub cpu_fallback_profile: CpuFallbackProfile,
}

impl SessionOptions {
    pub const fn new(model_name: &'static str) -> Self {
        Self {
            model_name,
            allow_acceleration: true,
            cpu_fallback_profile: CpuFallbackProfile::Default,
        }
    }

    pub const fn with_cpu_fallback_profile(mut self, profile: CpuFallbackProfile) -> Self {
        self.cpu_fallback_profile = profile;
        self
    }

    pub const fn cpu_only(mut self) -> Self {
        self.allow_acceleration = false;
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionProviderStatus {
    pub model_name: String,
    pub active_provider: String,
    pub degraded: bool,
}

fn provider_statuses() -> &'static Mutex<BTreeMap<String, ExecutionProviderStatus>> {
    static STATUSES: OnceLock<Mutex<BTreeMap<String, ExecutionProviderStatus>>> = OnceLock::new();
    STATUSES.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn publish_status(model_name: &str, active_provider: &str, degraded: bool) {
    if let Ok(mut statuses) = provider_statuses().lock() {
        statuses.insert(
            model_name.to_owned(),
            ExecutionProviderStatus {
                model_name: model_name.to_owned(),
                active_provider: active_provider.to_owned(),
                degraded,
            },
        );
    }
}

pub fn active_execution_providers() -> Vec<ExecutionProviderStatus> {
    provider_statuses()
        .lock()
        .map(|statuses| statuses.values().cloned().collect())
        .unwrap_or_default()
}

struct ProviderCandidate {
    name: &'static str,
    provider: ExecutionProviderDispatch,
    force_sequential: bool,
}

fn push_if_runtime_available<E>(
    providers: &mut Vec<ProviderCandidate>,
    unavailable: &mut Vec<String>,
    name: &'static str,
    provider: E,
    force_sequential: bool,
) where
    E: ExecutionProvider + Into<ExecutionProviderDispatch>,
{
    match provider.is_available() {
        Ok(true) => providers.push(ProviderCandidate {
            name,
            provider: provider.into(),
            force_sequential,
        }),
        Ok(false) => {
            let reason = format!("{name}: not advertised by the selected ONNX Runtime");
            log::info!("AI execution provider skipped: {reason}");
            unavailable.push(reason);
        }
        Err(error) => {
            let reason = format!("{name}: could not query provider availability: {error}");
            log::warn!("AI execution provider skipped: {reason}");
            unavailable.push(reason);
        }
    }
}

fn preferred_execution_providers() -> (Vec<ProviderCandidate>, Vec<String>) {
    let mut providers = Vec::new();
    let mut unavailable = Vec::new();

    #[cfg(target_os = "windows")]
    {
        push_if_runtime_available(
            &mut providers,
            &mut unavailable,
            "DirectML",
            ort::ep::DirectML::default(),
            true,
        );
        push_if_runtime_available(
            &mut providers,
            &mut unavailable,
            "CUDA",
            ort::ep::CUDA::default(),
            false,
        );
        push_if_runtime_available(
            &mut providers,
            &mut unavailable,
            "TensorRT",
            ort::ep::TensorRT::default(),
            false,
        );
    }

    #[cfg(target_os = "macos")]
    {
        push_if_runtime_available(
            &mut providers,
            &mut unavailable,
            "CoreML",
            ort::ep::CoreML::default(),
            false,
        );
    }

    #[cfg(target_os = "linux")]
    {
        push_if_runtime_available(
            &mut providers,
            &mut unavailable,
            "CUDA",
            ort::ep::CUDA::default(),
            false,
        );
        push_if_runtime_available(
            &mut providers,
            &mut unavailable,
            "TensorRT",
            ort::ep::TensorRT::default(),
            false,
        );
        push_if_runtime_available(
            &mut providers,
            &mut unavailable,
            "ROCm",
            ort::ep::ROCm::default(),
            false,
        );
    }

    #[cfg(target_os = "android")]
    {
        push_if_runtime_available(
            &mut providers,
            &mut unavailable,
            "NNAPI",
            ort::ep::NNAPI::default(),
            false,
        );
        push_if_runtime_available(
            &mut providers,
            &mut unavailable,
            "XNNPACK",
            ort::ep::XNNPACK::default(),
            false,
        );
    }

    (providers, unavailable)
}

fn configure_common_builder(mut builder: SessionBuilder) -> Result<SessionBuilder> {
    builder = builder
        .with_memory_pattern(false)
        .map_err(|error| anyhow::anyhow!("disable ONNX Runtime memory pattern: {error}"))?;
    Ok(builder)
}

fn configure_cpu_builder(
    mut builder: SessionBuilder,
    options: &SessionOptions,
) -> Result<SessionBuilder> {
    if options.cpu_fallback_profile == CpuFallbackProfile::WindowsSamEncoder
        && cfg!(target_os = "windows")
    {
        builder = builder
            .with_parallel_execution(false)
            .map_err(|error| {
                anyhow::anyhow!("force sequential Windows SAM CPU execution: {error}")
            })?
            .with_intra_threads(1)
            .map_err(|error| anyhow::anyhow!("limit Windows SAM CPU inference threads: {error}"))?
            .with_optimization_level(GraphOptimizationLevel::Disable)
            .map_err(|error| {
                anyhow::anyhow!("disable Windows SAM CPU graph optimizations: {error}")
            })?;
    }
    Ok(builder)
}

fn cpu_provider(options: &SessionOptions) -> ExecutionProviderDispatch {
    let disable_arena = cfg!(target_os = "android")
        || (cfg!(target_os = "windows")
            && options.cpu_fallback_profile == CpuFallbackProfile::WindowsSamEncoder);
    ort::ep::CPU::default()
        .with_arena_allocator(!disable_arena)
        .build()
}

fn commit_model(builder: &mut SessionBuilder, source: &ModelSource) -> Result<Session> {
    match source {
        ModelSource::Path(path) => builder
            .commit_from_file(path)
            .with_context(|| format!("load ONNX model from {}", path.display())),
        ModelSource::Bytes(bytes) => builder
            .commit_from_memory(bytes.as_ref())
            .context("load ONNX model from memory"),
    }
}

fn create_cpu_session_inner(source: &ModelSource, options: &SessionOptions) -> Result<Session> {
    let builder = Session::builder().context("create CPU ONNX Runtime session")?;
    let builder = configure_common_builder(builder)?;
    let mut builder = configure_cpu_builder(builder, options)?
        .with_execution_providers([cpu_provider(options).error_on_failure()])
        .map_err(|error| anyhow::anyhow!("configure ONNX CPU execution provider: {error}"))?;
    commit_model(&mut builder, source)
}

fn create_accelerated_session(
    source: &ModelSource,
    options: &SessionOptions,
    candidate: ProviderCandidate,
) -> Result<Session> {
    let builder = Session::builder()
        .with_context(|| format!("create {} ONNX Runtime session", candidate.name))?;
    let mut builder = configure_common_builder(builder)?;
    if candidate.force_sequential {
        builder = builder.with_parallel_execution(false).map_err(|error| {
            anyhow::anyhow!("configure {} sequential execution: {error}", candidate.name)
        })?;
    }
    let mut builder = builder
        .with_execution_providers([candidate.provider.error_on_failure(), cpu_provider(options)])
        .map_err(|error| {
            anyhow::anyhow!("configure {} execution provider: {error}", candidate.name)
        })?;
    commit_model(&mut builder, source)
}

pub struct FallbackSession {
    session: Session,
    source: ModelSource,
    options: SessionOptions,
    active_provider: &'static str,
    accelerated: bool,
    degraded: bool,
}

impl FallbackSession {
    pub fn run_with_fallback<T, F>(&mut self, operation: &str, mut run: F) -> Result<T>
    where
        F: FnMut(&mut Session, bool) -> Result<T>,
    {
        match run(&mut self.session, self.accelerated) {
            Ok(value) => Ok(value),
            Err(accelerated_error) if self.accelerated => {
                let failed_provider = self.active_provider;
                if is_gpu_memory_failure(&accelerated_error) {
                    quarantine_ai_gpu_after_memory_failure(
                        operation,
                        failed_provider,
                        &accelerated_error,
                    );
                    return Err(accelerated_error.context(format!(
                        "{operation} exceeded the GPU memory safety budget; the AI job was stopped before CPU fallback"
                    )));
                }
                log::warn!(
                    "{operation} failed on {failed_provider}; rebuilding {} on CPU: {accelerated_error:#}",
                    self.options.model_name
                );
                calibraw_core::diagnostics::record(format!(
                    "AI {}: {failed_provider} inference failed; switching to CPU fallback",
                    self.options.model_name
                ));
                self.degraded = true;
                publish_status(self.options.model_name, failed_provider, true);

                let cpu_session = create_cpu_session_inner(&self.source, &self.options)
                    .with_context(|| {
                        format!(
                            "rebuild {} on CPU after {failed_provider} inference failure: {accelerated_error:#}",
                            self.options.model_name
                        )
                    })?;
                self.session = cpu_session;
                self.active_provider = "CPU (fallback)";
                self.accelerated = false;
                self.degraded = true;
                publish_status(self.options.model_name, self.active_provider, true);

                log::info!(
                    "{} now uses CPU fallback after {failed_provider} runtime failure",
                    self.options.model_name
                );
                run(&mut self.session, false).with_context(|| {
                    format!(
                        "{operation} failed on {failed_provider} ({accelerated_error:#}) and again on CPU fallback"
                    )
                })
            }
            Err(error) => Err(error),
        }
    }
}

pub fn create_session_with_fallback(
    model: impl Into<ModelSource>,
    options: SessionOptions,
) -> Result<FallbackSession> {
    let source = model.into();
    let source_description = source.description();
    let mut attempted_acceleration = false;
    let mut setup_failures = Vec::new();

    let acceleration_enabled = options.allow_acceleration && ai_acceleration_enabled();
    if acceleration_enabled {
        let (providers, unavailable) = preferred_execution_providers();
        attempted_acceleration = !providers.is_empty() || !unavailable.is_empty();
        setup_failures.extend(unavailable);
        for candidate in providers {
            let provider_name = candidate.name;
            match create_accelerated_session(&source, &options, candidate) {
                Ok(session) => {
                    log::info!(
                        "AI {} uses {provider_name} for {}",
                        options.model_name,
                        source_description
                    );
                    calibraw_core::diagnostics::record(format!(
                        "AI {} execution provider: {provider_name}",
                        options.model_name
                    ));
                    publish_status(options.model_name, provider_name, false);
                    return Ok(FallbackSession {
                        session,
                        source,
                        options,
                        active_provider: provider_name,
                        accelerated: true,
                        degraded: false,
                    });
                }
                Err(error) => {
                    log::warn!(
                        "AI {} could not initialize {provider_name}; trying the next provider: {error:#}",
                        options.model_name
                    );
                    setup_failures.push(format!("{provider_name}: {error:#}"));
                }
            }
        }
    }

    let session = create_cpu_session_inner(&source, &options).with_context(|| {
        if setup_failures.is_empty() {
            format!("create CPU ONNX session for {}", options.model_name)
        } else {
            format!(
                "create CPU fallback for {} after accelerated providers failed: {}",
                options.model_name,
                setup_failures.join(" | ")
            )
        }
    })?;

    let active_provider = if attempted_acceleration {
        "CPU (fallback)"
    } else {
        "CPU"
    };
    let degraded = attempted_acceleration && !setup_failures.is_empty();
    if attempted_acceleration {
        log::info!(
            "AI {} uses CPU fallback for {} after accelerator setup failures",
            options.model_name,
            source_description
        );
        calibraw_core::diagnostics::record(format!(
            "AI {} execution provider: CPU (fallback)",
            options.model_name
        ));
    } else {
        let reason = if options.allow_acceleration && !acceleration_enabled {
            "GPU acceleration disabled in Settings"
        } else {
            "no accelerator configured for this target/session"
        };
        log::info!(
            "AI {} uses CPU for {} ({reason})",
            options.model_name,
            source_description
        );
        calibraw_core::diagnostics::record(format!(
            "AI {} execution provider: CPU",
            options.model_name
        ));
    }
    publish_status(options.model_name, active_provider, degraded);

    Ok(FallbackSession {
        session,
        source,
        options,
        active_provider,
        accelerated: false,
        degraded,
    })
}

#[cfg(test)]
mod tests {
    use super::{is_gpu_memory_failure, FallbackSession};

    #[test]
    fn fallback_session_preserves_thread_traits() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<FallbackSession>();
    }

    #[test]
    fn gpu_allocation_failures_are_quarantined_instead_of_retried() {
        let error =
            anyhow::anyhow!("BFCArena::AllocateRawInternal failed to allocate memory on CUDA GPU");
        assert!(is_gpu_memory_failure(&error));
        assert!(!is_gpu_memory_failure(&anyhow::anyhow!(
            "invalid tensor shape"
        )));
    }
}
