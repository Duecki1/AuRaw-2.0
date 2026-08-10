//! ONNX Runtime execution-provider selection with automatic CPU fallback.
//!
//! All provider probing and recovery lives in `auraw-ai`; UI crates only need
//! the lightweight diagnostics exposed by this module.

use anyhow::{Context, Result};
use ort::{
    ep::{ExecutionProvider, ExecutionProviderDispatch},
    session::{
        builder::{GraphOptimizationLevel, SessionBuilder},
        Session,
    },
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

/// Model storage used when a session must be rebuilt after a runtime EP failure.
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

/// CPU settings used only if an accelerated session cannot be used.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CpuFallbackProfile {
    #[default]
    Default,
    /// SAM 2.1's Hiera encoder has shown numerical instability with graph/layout
    /// fusions in some third-party Windows CPU runtimes. GPU sessions use normal
    /// optimizations; only the CPU recovery session uses these conservative flags.
    WindowsSamEncoder,
}

/// Options shared by initial session construction and any Tier-2 CPU rebuild.
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

/// Returns the most recently observed backend for each logical AI model.
///
/// This is intentionally UI-agnostic. `auraw-ui` may render or copy the values
/// into Settings > Diagnostics without importing ONNX Runtime itself.
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

/// Adds an EP only when the selected ONNX Runtime actually advertises it.
///
/// `ort` features compile the Rust registration glue; they do *not* guarantee
/// that a user-selected native runtime was built with the matching provider.
/// Probing `GetAvailableProviders` first prevents AuRaw from asking, for
/// example, a CUDA-only runtime to dlopen ROCm/TensorRT provider libraries that
/// are not part of that distribution.
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
        // DirectML is the widest-coverage Windows GPU path. Prefer CUDA next,
        // with TensorRT last because it has stricter external dependencies and
        // can have a much larger engine-build cost.
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
    // Dynamic AI tensors can otherwise cause ORT to retain large shape-specific
    // allocations. Keeping the memory pattern disabled matches AuRaw's existing
    // mobile/CPU safety behavior and makes GPU -> CPU recovery less bursty.
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

/// A session that remembers how to reconstruct itself on CPU after a runtime
/// accelerator failure. The wrapper remains `Send`/`Sync` whenever `ort::Session`
/// does; inference still requires `&mut self`, matching ORT's thread-safety model.
pub struct FallbackSession {
    session: Session,
    source: ModelSource,
    options: SessionOptions,
    active_provider: &'static str,
    accelerated: bool,
    degraded: bool,
}

impl FallbackSession {
    /// Human-readable provider used by Settings > Diagnostics and logs.
    pub const fn active_execution_provider(&self) -> &'static str {
        self.active_provider
    }

    pub const fn is_accelerated(&self) -> bool {
        self.accelerated
    }

    pub const fn is_degraded(&self) -> bool {
        self.degraded
    }

    /// Executes inference and transparently retries once on CPU if the current
    /// accelerated session fails. The closure receives whether the current
    /// attempt uses an accelerator and must be retryable; keep input tensors
    /// available for a possible second CPU invocation.
    pub fn run_with_fallback<T, F>(&mut self, operation: &str, mut run: F) -> Result<T>
    where
        F: FnMut(&mut Session, bool) -> Result<T>,
    {
        match run(&mut self.session, self.accelerated) {
            Ok(value) => Ok(value),
            Err(accelerated_error) if self.accelerated => {
                let failed_provider = self.active_provider;
                log::warn!(
                    "{operation} failed on {failed_provider}; rebuilding {} on CPU: {accelerated_error:#}",
                    self.options.model_name
                );
                auraw_core::diagnostics::record(format!(
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

/// Creates an ONNX Runtime session using the target-specific accelerator
/// priority list, then falls back to CPU if provider setup or model compilation
/// fails. Runtime inference failures are handled by [`FallbackSession::run_with_fallback`].
pub fn create_session_with_fallback(
    model: impl Into<ModelSource>,
    options: SessionOptions,
) -> Result<FallbackSession> {
    let source = model.into();
    let source_description = source.description();
    let mut attempted_acceleration = false;
    let mut setup_failures = Vec::new();

    if options.allow_acceleration {
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
                    auraw_core::diagnostics::record(format!(
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
        auraw_core::diagnostics::record(format!(
            "AI {} execution provider: CPU (fallback)",
            options.model_name
        ));
    } else {
        log::info!(
            "AI {} uses CPU for {} (no accelerator configured for this target/session)",
            options.model_name,
            source_description
        );
        auraw_core::diagnostics::record(format!(
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
    use super::FallbackSession;

    #[test]
    fn fallback_session_preserves_thread_traits() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<FallbackSession>();
    }
}
