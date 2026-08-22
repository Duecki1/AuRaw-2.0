
use crate::execution_provider::{
    ai_acceleration_enabled, create_session_with_fallback, FallbackSession, ModelSource,
    SessionOptions,
};
use anyhow::Result;
use std::{
    ops::{Deref, DerefMut},
    sync::{
        atomic::{AtomicU64, AtomicU8, Ordering},
        Mutex, MutexGuard, OnceLock, TryLockError,
    },
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AiModel {
    BiRefNetLow,
    BiRefNetMedium,
    BiRefNetHigh,
    ViTMatte,
    SamEncoder,
    SamDecoder,
    BigLama,
    RawNindBayer,
    RawNindLinear,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AiRuntimeContext {
    Masks,
    Remove,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModelRetention {
    Interactive(AiRuntimeContext),
    OneShot,
}

struct ActiveModel<S> {
    model: AiModel,
    retention: ModelRetention,
    provider_generation: u64,
    acceleration_enabled: bool,
    session: S,
}

struct RuntimeSlot<S> {
    active: Option<ActiveModel<S>>,
}

impl<S> Default for RuntimeSlot<S> {
    fn default() -> Self {
        Self { active: None }
    }
}

impl<S> RuntimeSlot<S> {
    fn ensure_model<E>(
        &mut self,
        model: AiModel,
        retention: ModelRetention,
        provider_generation: u64,
        acceleration_enabled: bool,
        create: impl FnOnce() -> std::result::Result<S, E>,
    ) -> std::result::Result<&mut S, E> {
        let reusable = self.active.as_ref().is_some_and(|active| {
            active.model == model
                && active.provider_generation == provider_generation
                && active.acceleration_enabled == acceleration_enabled
        });
        if !reusable {
            if let Some(active) = self.active.take() {
                log::info!("unloading cached AI model session: {:?}", active.model);
                drop(active);
            }
            let session = create()?;
            log::info!("loaded AI model session: {model:?}");
            self.active = Some(ActiveModel {
                model,
                retention,
                provider_generation,
                acceleration_enabled,
                session,
            });
        } else if let Some(active) = self.active.as_mut() {
            active.retention = retention;
        }
        Ok(&mut self.active.as_mut().expect("AI session was just created").session)
    }

    fn reconcile(
        &mut self,
        active_context: Option<AiRuntimeContext>,
        provider_generation: u64,
        acceleration_enabled: bool,
    ) {
        let retain = self.active.as_ref().is_some_and(|active| {
            active.provider_generation == provider_generation
                && active.acceleration_enabled == acceleration_enabled
                && matches!(
                    active.retention,
                    ModelRetention::Interactive(context) if Some(context) == active_context
                )
        });
        if !retain {
            if let Some(active) = self.active.take() {
                log::info!("unloading AI model session: {:?}", active.model);
                drop(active);
            }
        }
    }

    #[cfg(test)]
    fn active_model(&self) -> Option<AiModel> {
        self.active.as_ref().map(|active| active.model)
    }
}

fn runtime() -> &'static Mutex<RuntimeSlot<FallbackSession>> {
    static RUNTIME: OnceLock<Mutex<RuntimeSlot<FallbackSession>>> = OnceLock::new();
    RUNTIME.get_or_init(|| Mutex::new(RuntimeSlot::default()))
}

const CONTEXT_NONE: u8 = 0;
const CONTEXT_MASKS: u8 = 1;
const CONTEXT_REMOVE: u8 = 2;
static ACTIVE_CONTEXT: AtomicU8 = AtomicU8::new(CONTEXT_NONE);
static PROVIDER_GENERATION: AtomicU64 = AtomicU64::new(0);

fn encode_context(context: Option<AiRuntimeContext>) -> u8 {
    match context {
        None => CONTEXT_NONE,
        Some(AiRuntimeContext::Masks) => CONTEXT_MASKS,
        Some(AiRuntimeContext::Remove) => CONTEXT_REMOVE,
    }
}

fn active_context() -> Option<AiRuntimeContext> {
    match ACTIVE_CONTEXT.load(Ordering::Acquire) {
        CONTEXT_MASKS => Some(AiRuntimeContext::Masks),
        CONTEXT_REMOVE => Some(AiRuntimeContext::Remove),
        _ => None,
    }
}

fn lock_runtime() -> MutexGuard<'static, RuntimeSlot<FallbackSession>> {
    runtime()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn try_reconcile<S>(
    runtime: &Mutex<RuntimeSlot<S>>,
    context: Option<AiRuntimeContext>,
    provider_generation: u64,
    acceleration_enabled: bool,
) -> bool {
    match runtime.try_lock() {
        Ok(mut slot) => {
            slot.reconcile(context, provider_generation, acceleration_enabled);
            true
        }
        Err(TryLockError::Poisoned(error)) => {
            let mut slot = error.into_inner();
            slot.reconcile(context, provider_generation, acceleration_enabled);
            true
        }
        Err(TryLockError::WouldBlock) => false,
    }
}

pub fn set_active_ai_context(context: Option<AiRuntimeContext>) {
    ACTIVE_CONTEXT.store(encode_context(context), Ordering::Release);
    let _ = try_reconcile(
        runtime(),
        context,
        PROVIDER_GENERATION.load(Ordering::Acquire),
        ai_acceleration_enabled(),
    );
}

pub(crate) fn invalidate_for_provider_change() {
    let generation = PROVIDER_GENERATION.fetch_add(1, Ordering::AcqRel) + 1;
    let _ = try_reconcile(
        runtime(),
        active_context(),
        generation,
        ai_acceleration_enabled(),
    );
}

pub(crate) struct ModelSessionGuard {
    runtime: Option<MutexGuard<'static, RuntimeSlot<FallbackSession>>>,
}

impl Deref for ModelSessionGuard {
    type Target = FallbackSession;

    fn deref(&self) -> &Self::Target {
        &self
            .runtime
            .as_ref()
            .expect("AI model session lease was already released")
            .active
            .as_ref()
            .expect("AI model session lease has no active session")
            .session
    }
}

impl DerefMut for ModelSessionGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self
            .runtime
            .as_mut()
            .expect("AI model session lease was already released")
            .active
            .as_mut()
            .expect("AI model session lease has no active session")
            .session
    }
}

impl Drop for ModelSessionGuard {
    fn drop(&mut self) {
        let Some(mut guard) = self.runtime.take() else {
            return;
        };
        guard.reconcile(
            active_context(),
            PROVIDER_GENERATION.load(Ordering::Acquire),
            ai_acceleration_enabled(),
        );
        drop(guard);

        let _ = try_reconcile(
            runtime(),
            active_context(),
            PROVIDER_GENERATION.load(Ordering::Acquire),
            ai_acceleration_enabled(),
        );
    }
}

pub(crate) fn acquire_model_session(
    model: AiModel,
    source: impl Into<ModelSource>,
    options: SessionOptions,
    retention: ModelRetention,
) -> Result<ModelSessionGuard> {
    let source = source.into();
    let mut runtime = lock_runtime();
    let provider_generation = PROVIDER_GENERATION.load(Ordering::Acquire);
    let acceleration_enabled = ai_acceleration_enabled();
    runtime.ensure_model(
        model,
        retention,
        provider_generation,
        acceleration_enabled,
        || create_session_with_fallback(source, options),
    )?;
    Ok(ModelSessionGuard {
        runtime: Some(runtime),
    })
}

pub(crate) fn with_model_session<T>(
    model: AiModel,
    source: impl Into<ModelSource>,
    options: SessionOptions,
    retention: ModelRetention,
    run: impl FnOnce(&mut FallbackSession) -> Result<T>,
) -> Result<T> {
    let mut session = acquire_model_session(model, source, options, retention)?;
    run(&mut session)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        sync::{Arc, Barrier},
        thread,
        time::{Duration, Instant},
    };

    #[derive(Clone)]
    struct DropLog {
        label: &'static str,
        events: Arc<Mutex<Vec<String>>>,
    }

    impl Drop for DropLog {
        fn drop(&mut self) {
            self.events
                .lock()
                .unwrap()
                .push(format!("drop {}", self.label));
        }
    }

    fn interactive_masks() -> ModelRetention {
        ModelRetention::Interactive(AiRuntimeContext::Masks)
    }

    #[test]
    fn same_model_is_reused() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut slot = RuntimeSlot::default();
        slot.ensure_model(AiModel::BiRefNetLow, interactive_masks(), 0, true, || {
            events.lock().unwrap().push("create low".to_owned());
            Ok::<_, ()>(DropLog {
                label: "low",
                events: Arc::clone(&events),
            })
        })
        .unwrap();
        slot.ensure_model(AiModel::BiRefNetLow, interactive_masks(), 0, true, || {
            events.lock().unwrap().push("create low again".to_owned());
            Ok::<_, ()>(DropLog {
                label: "low again",
                events: Arc::clone(&events),
            })
        })
        .unwrap();
        assert_eq!(slot.active_model(), Some(AiModel::BiRefNetLow));
        assert_eq!(&*events.lock().unwrap(), &["create low".to_owned()]);
    }


    #[test]
    fn only_one_session_can_be_resident() {
        let mut slot = RuntimeSlot::default();
        slot.ensure_model(AiModel::BiRefNetMedium, interactive_masks(), 0, true, || {
            Ok::<_, ()>(())
        })
        .unwrap();
        assert_eq!(slot.active_model(), Some(AiModel::BiRefNetMedium));
        slot.ensure_model(AiModel::SamEncoder, interactive_masks(), 0, true, || {
            Ok::<_, ()>(())
        })
        .unwrap();
        assert_eq!(slot.active_model(), Some(AiModel::SamEncoder));
    }

    #[test]
    fn leaving_ai_context_requests_unload() {
        let mut slot = RuntimeSlot::default();
        slot.ensure_model(AiModel::ViTMatte, interactive_masks(), 0, true, || {
            Ok::<_, ()>(())
        })
        .unwrap();
        slot.reconcile(None, 0, true);
        assert_eq!(slot.active_model(), None);
    }

    #[test]
    fn one_shot_model_unloads_after_inference() {
        let mut slot = RuntimeSlot::default();
        slot.ensure_model(AiModel::RawNindBayer, ModelRetention::OneShot, 0, true, || {
            Ok::<_, ()>(())
        })
        .unwrap();
        slot.reconcile(Some(AiRuntimeContext::Masks), 0, true);
        assert_eq!(slot.active_model(), None);
    }

    #[test]
    fn tab_switch_unload_request_does_not_block() {
        let runtime = Arc::new(Mutex::new(RuntimeSlot::<()>::default()));
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let worker_runtime = Arc::clone(&runtime);
        let worker_entered = Arc::clone(&entered);
        let worker_release = Arc::clone(&release);
        let worker = thread::spawn(move || {
            let _guard = worker_runtime.lock().unwrap();
            worker_entered.wait();
            worker_release.wait();
        });
        entered.wait();
        let start = Instant::now();
        assert!(!try_reconcile(&runtime, None, 0, true));
        assert!(start.elapsed() < Duration::from_millis(100));
        release.wait();
        worker.join().unwrap();
    }

    #[test]
    fn pending_unload_is_applied_after_inference_releases_session() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let runtime = Arc::new(Mutex::new(RuntimeSlot::<DropLog>::default()));
        let context = Arc::new(AtomicU8::new(CONTEXT_REMOVE));
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));

        let worker_runtime = Arc::clone(&runtime);
        let worker_context = Arc::clone(&context);
        let worker_events = Arc::clone(&events);
        let worker_entered = Arc::clone(&entered);
        let worker_release = Arc::clone(&release);
        let worker = thread::spawn(move || {
            let mut slot = worker_runtime.lock().unwrap();
            slot.ensure_model(
                AiModel::BigLama,
                ModelRetention::Interactive(AiRuntimeContext::Remove),
                0,
                true,
                || {
                    Ok::<_, ()>(DropLog {
                        label: "big-lama",
                        events: worker_events,
                    })
                },
            )
            .unwrap();
            worker_entered.wait();
            worker_release.wait();
            let active_context = match worker_context.load(Ordering::Acquire) {
                CONTEXT_REMOVE => Some(AiRuntimeContext::Remove),
                CONTEXT_MASKS => Some(AiRuntimeContext::Masks),
                _ => None,
            };
            slot.reconcile(active_context, 0, true);
        });

        entered.wait();
        context.store(CONTEXT_NONE, Ordering::Release);
        assert!(!try_reconcile(&runtime, None, 0, true));
        release.wait();
        worker.join().unwrap();
        assert_eq!(runtime.lock().unwrap().active_model(), None);
        assert_eq!(&*events.lock().unwrap(), &["drop big-lama".to_owned()]);
    }

    #[test]
    fn changing_model_variant_replaces_old_model() {
        let mut slot = RuntimeSlot::default();
        slot.ensure_model(AiModel::BiRefNetLow, interactive_masks(), 0, true, || {
            Ok::<_, ()>(())
        })
        .unwrap();
        slot.ensure_model(AiModel::BiRefNetHigh, interactive_masks(), 0, true, || {
            Ok::<_, ()>(())
        })
        .unwrap();
        assert_eq!(slot.active_model(), Some(AiModel::BiRefNetHigh));
    }

}
