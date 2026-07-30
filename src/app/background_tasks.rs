use std::collections::VecDeque;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct TaskId(u64);

impl TaskId {
    pub(crate) const fn get(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum TaskKind {
    SingleExport,
    LibraryBatchExport,
    LensCorrection {
        document_id: u64,
        generation: u64,
    },
    SubjectMask {
        document_id: u64,
        generation: u64,
    },
    ObjectMask {
        document_id: u64,
        generation: u64,
    },
    LandscapeMask {
        document_id: u64,
        generation: u64,
    },
    Inpainting {
        document_id: u64,
        generation: u64,
    },
    LibraryAiMaskRefresh,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TaskStatus {
    Queued,
    Running,
    Cancelling,
    Failed,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum TaskProgressValue {
    Indeterminate,
    Fraction(f32),
    Units {
        completed: u64,
        total: u64,
        unit: Option<String>,
    },
}

impl TaskProgressValue {
    pub(crate) fn fraction(&self) -> Option<f32> {
        match self {
            Self::Indeterminate => None,
            Self::Fraction(value) => Some(value.clamp(0.0, 1.0)),
            Self::Units {
                completed, total, ..
            } => (*total > 0).then(|| (*completed as f32 / *total as f32).clamp(0.0, 1.0)),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TaskProgress {
    pub(crate) value: TaskProgressValue,
    pub(crate) phase: String,
    pub(crate) detail: Option<String>,
}

impl TaskProgress {
    pub(crate) fn indeterminate(phase: impl Into<String>) -> Self {
        Self {
            value: TaskProgressValue::Indeterminate,
            phase: phase.into(),
            detail: None,
        }
    }

    pub(crate) fn fraction(fraction: f32, phase: impl Into<String>) -> Self {
        Self {
            value: TaskProgressValue::Fraction(fraction.clamp(0.0, 1.0)),
            phase: phase.into(),
            detail: None,
        }
    }

    pub(crate) fn units(
        completed: u64,
        total: u64,
        unit: impl Into<Option<String>>,
        phase: impl Into<String>,
    ) -> Self {
        Self {
            value: TaskProgressValue::Units {
                completed,
                total,
                unit: unit.into(),
            },
            phase: phase.into(),
            detail: None,
        }
    }

    pub(crate) fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

#[derive(Clone, Debug)]
pub(crate) struct TaskSnapshot {
    pub(crate) id: TaskId,
    pub(crate) kind: TaskKind,
    pub(crate) name: String,
    pub(crate) status: TaskStatus,
    pub(crate) progress: TaskProgress,
    pub(crate) error: Option<String>,
    pub(crate) details_open: bool,
}

struct BackgroundTask {
    id: TaskId,
    kind: TaskKind,
    name: String,
    status: TaskStatus,
    progress: TaskProgress,
    error: Option<String>,
    details_open: bool,
    global_visible: bool,
    cancellation: Arc<AtomicBool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CancelTaskResult {
    NotFound,
    RemovedQueued,
    CancellationRequested,
    DismissedFailure,
}

#[derive(Default)]
pub(crate) struct BackgroundTaskManager {
    next_id: u64,
    current: Option<TaskId>,
    queue: VecDeque<TaskId>,
    tasks: Vec<BackgroundTask>,
}

impl BackgroundTaskManager {
    fn insert_task(
        &mut self,
        kind: TaskKind,
        name: impl Into<String>,
        status: TaskStatus,
        initial_progress: TaskProgress,
        details_open: bool,
        global_visible: bool,
    ) -> TaskId {
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let id = TaskId(self.next_id);
        self.tasks.push(BackgroundTask {
            id,
            kind,
            name: name.into(),
            status,
            progress: initial_progress,
            error: None,
            details_open,
            global_visible,
            cancellation: Arc::new(AtomicBool::new(false)),
        });
        id
    }

    pub(crate) fn enqueue(
        &mut self,
        kind: TaskKind,
        name: impl Into<String>,
        initial_progress: TaskProgress,
        details_open: bool,
    ) -> TaskId {
        let id = self.insert_task(
            kind,
            name,
            TaskStatus::Queued,
            initial_progress,
            details_open,
            true,
        );
        self.queue.push_back(id);
        id
    }

    /// Register an operation that should run immediately without occupying the
    /// serialized background-work slot. This is used for latency-sensitive local
    /// AI inference after all required model files are already present.
    pub(crate) fn start_nonblocking(
        &mut self,
        kind: TaskKind,
        name: impl Into<String>,
        initial_progress: TaskProgress,
        details_open: bool,
    ) -> TaskId {
        self.insert_task(
            kind,
            name,
            TaskStatus::Running,
            initial_progress,
            details_open,
            false,
        )
    }

    pub(crate) fn enqueue_coalesced_lens(
        &mut self,
        document_id: u64,
        generation: u64,
        name: impl Into<String>,
        initial_progress: TaskProgress,
        details_open: bool,
    ) -> (TaskId, Vec<TaskId>) {
        let obsolete = self
            .tasks
            .iter()
            .filter(|task| {
                task.status == TaskStatus::Queued
                    && matches!(
                        &task.kind,
                        TaskKind::LensCorrection {
                            document_id: existing,
                            ..
                        } if *existing == document_id
                    )
            })
            .map(|task| task.id)
            .collect::<Vec<_>>();
        for id in &obsolete {
            self.remove_task(*id);
        }

        if let Some(current) = self.current {
            let same_document = self.task(current).is_some_and(|task| {
                matches!(
                    &task.kind,
                    TaskKind::LensCorrection {
                        document_id: existing,
                        ..
                    } if *existing == document_id
                )
            });
            if same_document {
                let _ = self.request_cancel(current);
            }
        }

        let id = self.enqueue(
            TaskKind::LensCorrection {
                document_id,
                generation,
            },
            name,
            initial_progress,
            details_open,
        );
        (id, obsolete)
    }

    pub(crate) fn start_next(&mut self) -> Option<TaskId> {
        if self.current.is_some() {
            return None;
        }
        while let Some(id) = self.queue.pop_front() {
            let Some(task) = self.task_mut(id) else {
                continue;
            };
            if task.status != TaskStatus::Queued {
                continue;
            }
            task.status = TaskStatus::Running;
            self.current = Some(id);
            return Some(id);
        }
        None
    }

    /// Release the FIFO execution slot while keeping the task alive and
    /// cancellable. The worker continues independently and must later call
    /// `complete` or `fail` with the same stable task ID.
    pub(crate) fn release_current(&mut self, id: TaskId) -> bool {
        if self.current != Some(id) {
            return false;
        }
        let running = self.task(id).is_some_and(|task| {
            matches!(task.status, TaskStatus::Running | TaskStatus::Cancelling)
        });
        if !running {
            return false;
        }
        self.current = None;
        true
    }

    pub(crate) fn current_id(&self) -> Option<TaskId> {
        self.current
    }

    fn active_task(&self, global_only: bool) -> Option<&BackgroundTask> {
        self.current
            .and_then(|id| self.task(id))
            .filter(|task| !global_only || task.global_visible)
            .or_else(|| {
                self.tasks.iter().find(|task| {
                    (!global_only || task.global_visible)
                        && matches!(task.status, TaskStatus::Running | TaskStatus::Cancelling)
                        && !self.queue.contains(&task.id)
                })
            })
    }

    pub(crate) fn global_current_snapshot(&self) -> Option<TaskSnapshot> {
        self.active_task(true).map(Self::snapshot_from_task)
    }

    pub(crate) fn global_primary_snapshot_and_waiting_count(
        &self,
    ) -> (Option<TaskSnapshot>, usize) {
        let primary = self.active_task(true).or_else(|| {
            self.queue.iter().find_map(|id| {
                self.task(*id).filter(|task| {
                    task.global_visible && task.status == TaskStatus::Queued
                })
            })
        });
        let displayed_queued = if primary.is_some_and(|task| task.status == TaskStatus::Queued) {
            1
        } else {
            0
        };
        let waiting = self.global_queued_count().saturating_sub(displayed_queued);
        (primary.map(Self::snapshot_from_task), waiting)
    }

    pub(crate) fn snapshot(&self, id: TaskId) -> Option<TaskSnapshot> {
        self.task(id).map(Self::snapshot_from_task)
    }

    fn ordered_snapshots(&self, global_only: bool) -> Vec<TaskSnapshot> {
        let mut snapshots = Vec::new();
        let active_id = self.active_task(global_only).map(|active| {
            snapshots.push(Self::snapshot_from_task(active));
            active.id
        });
        snapshots.extend(
            self.tasks
                .iter()
                .filter(|task| {
                    (!global_only || task.global_visible)
                        && matches!(task.status, TaskStatus::Running | TaskStatus::Cancelling)
                        && Some(task.id) != active_id
                        && !self.queue.contains(&task.id)
                })
                .map(Self::snapshot_from_task),
        );
        for id in &self.queue {
            if let Some(task) = self
                .task(*id)
                .filter(|task| !global_only || task.global_visible)
            {
                snapshots.push(Self::snapshot_from_task(task));
            }
        }
        snapshots.extend(
            self.tasks
                .iter()
                .filter(|task| {
                    task.status == TaskStatus::Failed && (!global_only || task.global_visible)
                })
                .map(Self::snapshot_from_task),
        );
        snapshots
    }

    pub(crate) fn snapshots(&self) -> Vec<TaskSnapshot> {
        self.ordered_snapshots(false)
    }

    pub(crate) fn global_snapshots(&self) -> Vec<TaskSnapshot> {
        self.ordered_snapshots(true)
    }

    pub(crate) fn queued_count(&self) -> usize {
        self.queue
            .iter()
            .filter(|id| {
                self.task(**id)
                    .is_some_and(|task| task.status == TaskStatus::Queued)
            })
            .count()
    }

    pub(crate) fn global_queued_count(&self) -> usize {
        self.queue
            .iter()
            .filter(|id| {
                self.task(**id).is_some_and(|task| {
                    task.status == TaskStatus::Queued && task.global_visible
                })
            })
            .count()
    }

    pub(crate) fn has_visible_tasks(&self) -> bool {
        self.queued_count() > 0
            || self.has_failures()
            || self.tasks.iter().any(|task| {
                matches!(task.status, TaskStatus::Running | TaskStatus::Cancelling)
            })
    }

    pub(crate) fn has_global_visible_tasks(&self) -> bool {
        self.tasks.iter().any(|task| {
            task.global_visible
                && matches!(
                    task.status,
                    TaskStatus::Queued
                        | TaskStatus::Running
                        | TaskStatus::Cancelling
                        | TaskStatus::Failed
                )
        })
    }

    pub(crate) fn has_failures(&self) -> bool {
        self.tasks
            .iter()
            .any(|task| task.status == TaskStatus::Failed)
    }

    pub(crate) fn cancellation_token(&self, id: TaskId) -> Option<Arc<AtomicBool>> {
        self.task(id).map(|task| Arc::clone(&task.cancellation))
    }

    pub(crate) fn cancellation_requested(&self, id: TaskId) -> bool {
        self.task(id)
            .is_some_and(|task| task.cancellation.load(Ordering::Acquire))
    }

    pub(crate) fn update_progress(&mut self, id: TaskId, progress: TaskProgress) -> bool {
        let Some(task) = self.task_mut(id) else {
            return false;
        };
        if !matches!(task.status, TaskStatus::Running | TaskStatus::Cancelling) {
            return false;
        }
        task.progress = progress;
        if task.status == TaskStatus::Cancelling {
            task.progress.phase = "Cancelling…".to_owned();
            task.progress.detail = Some(
                "The current safe phase may finish before the task stops.".to_owned(),
            );
        }
        true
    }

    pub(crate) fn rename(&mut self, id: TaskId, name: impl Into<String>) -> bool {
        let Some(task) = self.task_mut(id) else {
            return false;
        };
        if matches!(
            task.status,
            TaskStatus::Running | TaskStatus::Cancelling | TaskStatus::Failed
        ) {
            task.name = name.into();
            true
        } else {
            false
        }
    }

    pub(crate) fn request_cancel(&mut self, id: TaskId) -> CancelTaskResult {
        let Some(status) = self.task(id).map(|task| task.status) else {
            return CancelTaskResult::NotFound;
        };
        match status {
            TaskStatus::Queued => {
                self.remove_task(id);
                CancelTaskResult::RemovedQueued
            }
            TaskStatus::Running | TaskStatus::Cancelling => {
                if let Some(task) = self.task_mut(id) {
                    task.status = TaskStatus::Cancelling;
                    task.progress.phase = "Cancelling…".to_owned();
                    task.progress.detail = Some(
                        "The current safe phase may finish before the task stops.".to_owned(),
                    );
                    task.cancellation.store(true, Ordering::Release);
                }
                CancelTaskResult::CancellationRequested
            }
            TaskStatus::Failed => {
                self.remove_task(id);
                CancelTaskResult::DismissedFailure
            }
        }
    }

    pub(crate) fn complete(&mut self, id: TaskId) -> bool {
        let terminal = self.task(id).is_some_and(|task| {
            matches!(task.status, TaskStatus::Running | TaskStatus::Cancelling)
        });
        if !terminal {
            return false;
        }
        if self.current == Some(id) {
            self.current = None;
        }
        self.remove_task(id);
        true
    }

    pub(crate) fn fail(&mut self, id: TaskId, error: impl Into<String>) -> bool {
        let runnable = self.task(id).is_some_and(|task| {
            matches!(task.status, TaskStatus::Running | TaskStatus::Cancelling)
        });
        if !runnable {
            return false;
        }
        let cancelling = self
            .task(id)
            .is_some_and(|task| task.status == TaskStatus::Cancelling);
        if self.current == Some(id) {
            self.current = None;
        }
        if cancelling {
            self.remove_task(id);
            return true;
        }
        let Some(task) = self.task_mut(id) else {
            return false;
        };
        task.status = TaskStatus::Failed;
        task.error = Some(error.into());
        task.progress = TaskProgress::indeterminate("Failed");
        task.details_open = true;
        true
    }

    pub(crate) fn set_global_visible(&mut self, id: TaskId, visible: bool) -> bool {
        let Some(task) = self.task_mut(id) else {
            return false;
        };
        task.global_visible = visible;
        true
    }

    pub(crate) fn set_details_open(&mut self, id: TaskId, open: bool) -> bool {
        let Some(task) = self.task_mut(id) else {
            return false;
        };
        task.details_open = open;
        true
    }

    pub(crate) fn details_open(&self, id: TaskId) -> bool {
        self.task(id).is_some_and(|task| task.details_open)
    }

    pub(crate) fn dismiss_failure(&mut self, id: TaskId) -> bool {
        if self
            .task(id)
            .is_some_and(|task| task.status == TaskStatus::Failed)
        {
            self.remove_task(id);
            true
        } else {
            false
        }
    }

    fn task(&self, id: TaskId) -> Option<&BackgroundTask> {
        self.tasks.iter().find(|task| task.id == id)
    }

    fn task_mut(&mut self, id: TaskId) -> Option<&mut BackgroundTask> {
        self.tasks.iter_mut().find(|task| task.id == id)
    }

    fn remove_task(&mut self, id: TaskId) {
        self.queue.retain(|queued| *queued != id);
        self.tasks.retain(|task| task.id != id);
        if self.current == Some(id) {
            self.current = None;
        }
    }

    fn snapshot_from_task(task: &BackgroundTask) -> TaskSnapshot {
        TaskSnapshot {
            id: task.id,
            kind: task.kind.clone(),
            name: task.name.clone(),
            status: task.status,
            progress: task.progress.clone(),
            error: task.error.clone(),
            details_open: task.details_open,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queued(manager: &mut BackgroundTaskManager, name: &str) -> TaskId {
        manager.enqueue(
            TaskKind::SingleExport,
            name,
            TaskProgress::indeterminate("Waiting"),
            false,
        )
    }

    #[test]
    fn fifo_ordering() {
        let mut manager = BackgroundTaskManager::default();
        let first = queued(&mut manager, "first");
        let second = queued(&mut manager, "second");
        assert_eq!(manager.start_next(), Some(first));
        assert!(manager.complete(first));
        assert_eq!(manager.start_next(), Some(second));
    }

    #[test]
    fn starts_next_after_completion() {
        let mut manager = BackgroundTaskManager::default();
        let first = queued(&mut manager, "first");
        let second = queued(&mut manager, "second");
        assert_eq!(manager.start_next(), Some(first));
        assert!(manager.complete(first));
        assert_eq!(manager.current_id(), None);
        assert_eq!(manager.start_next(), Some(second));
        assert_eq!(manager.current_id(), Some(second));
    }

    #[test]
    fn queued_cancel_removes_immediately() {
        let mut manager = BackgroundTaskManager::default();
        let first = queued(&mut manager, "first");
        let second = queued(&mut manager, "second");
        assert_eq!(manager.request_cancel(second), CancelTaskResult::RemovedQueued);
        assert!(manager.snapshot(second).is_none());
        assert_eq!(manager.start_next(), Some(first));
        assert_eq!(manager.queued_count(), 0);
    }

    #[test]
    fn running_cancel_is_cooperative() {
        let mut manager = BackgroundTaskManager::default();
        let id = queued(&mut manager, "running");
        assert_eq!(manager.start_next(), Some(id));
        let token = manager.cancellation_token(id).unwrap();
        assert_eq!(
            manager.request_cancel(id),
            CancelTaskResult::CancellationRequested
        );
        assert_eq!(manager.snapshot(id).unwrap().status, TaskStatus::Cancelling);
        assert!(token.load(Ordering::Acquire));
        assert_eq!(manager.current_id(), Some(id));
    }

    #[test]
    fn queued_count_excludes_current_and_failures() {
        let mut manager = BackgroundTaskManager::default();
        let first = queued(&mut manager, "first");
        let _second = queued(&mut manager, "second");
        let _third = queued(&mut manager, "third");
        assert_eq!(manager.start_next(), Some(first));
        assert_eq!(manager.queued_count(), 2);
        assert!(manager.fail(first, "boom"));
        assert_eq!(manager.queued_count(), 2);
    }

    #[test]
    fn progress_updates_use_stable_id() {
        let mut manager = BackgroundTaskManager::default();
        let first = queued(&mut manager, "first");
        let second = queued(&mut manager, "second");
        assert_eq!(manager.start_next(), Some(first));
        assert!(!manager.update_progress(second, TaskProgress::fraction(0.5, "Wrong")));
        assert!(manager.update_progress(first, TaskProgress::fraction(0.5, "Half")));
        assert_eq!(
            manager.snapshot(first).unwrap().progress.value,
            TaskProgressValue::Fraction(0.5)
        );
    }

    #[test]
    fn stale_updates_are_ignored_after_completion() {
        let mut manager = BackgroundTaskManager::default();
        let id = queued(&mut manager, "first");
        assert_eq!(manager.start_next(), Some(id));
        assert!(manager.complete(id));
        assert!(!manager.update_progress(id, TaskProgress::fraction(1.0, "Late")));
    }

    #[test]
    fn active_lens_task_is_cancelled_when_new_generation_is_queued() {
        let mut manager = BackgroundTaskManager::default();
        let (old, _) = manager.enqueue_coalesced_lens(
            7,
            1,
            "old",
            TaskProgress::indeterminate("Waiting"),
            false,
        );
        assert_eq!(manager.start_next(), Some(old));
        let token = manager.cancellation_token(old).unwrap();

        let (new, removed) = manager.enqueue_coalesced_lens(
            7,
            2,
            "new",
            TaskProgress::indeterminate("Waiting"),
            false,
        );

        assert!(removed.is_empty());
        assert_eq!(manager.snapshot(old).unwrap().status, TaskStatus::Cancelling);
        assert!(token.load(Ordering::Acquire));
        assert_eq!(manager.snapshot(new).unwrap().status, TaskStatus::Queued);
        assert_eq!(manager.queued_count(), 1);
    }

    #[test]
    fn obsolete_lens_tasks_are_coalesced() {
        let mut manager = BackgroundTaskManager::default();
        let (old, removed) = manager.enqueue_coalesced_lens(
            7,
            1,
            "old",
            TaskProgress::indeterminate("Waiting"),
            false,
        );
        assert!(removed.is_empty());
        let other = manager.enqueue(
            TaskKind::LensCorrection {
                document_id: 8,
                generation: 1,
            },
            "other",
            TaskProgress::indeterminate("Waiting"),
            false,
        );
        let (new, removed) = manager.enqueue_coalesced_lens(
            7,
            2,
            "new",
            TaskProgress::indeterminate("Waiting"),
            false,
        );
        assert_eq!(removed, vec![old]);
        assert!(manager.snapshot(old).is_none());
        assert!(manager.snapshot(other).is_some());
        assert!(manager.snapshot(new).is_some());
        assert_eq!(manager.queued_count(), 2);
    }
    #[test]
    fn nonblocking_task_does_not_occupy_fifo_slot() {
        let mut manager = BackgroundTaskManager::default();
        let export = queued(&mut manager, "export");
        assert_eq!(manager.start_next(), Some(export));
        let inference = manager.start_nonblocking(
            TaskKind::SubjectMask {
                document_id: 1,
                generation: 1,
            },
            "subject inference",
            TaskProgress::indeterminate("Inferencing"),
            true,
        );
        assert_eq!(manager.current_id(), Some(export));
        assert_eq!(manager.snapshot(inference).unwrap().status, TaskStatus::Running);
        assert!(manager.update_progress(
            inference,
            TaskProgress::indeterminate("Still inferencing")
        ));
        assert!(manager.complete(inference));
        assert_eq!(manager.current_id(), Some(export));
    }

    #[test]
    fn released_download_allows_next_fifo_task_while_inference_continues() {
        let mut manager = BackgroundTaskManager::default();
        let download = queued(&mut manager, "download");
        let export = queued(&mut manager, "export");
        assert_eq!(manager.start_next(), Some(download));
        assert!(manager.release_current(download));
        assert_eq!(manager.start_next(), Some(export));
        assert!(manager.update_progress(
            download,
            TaskProgress::indeterminate("Inferencing")
        ));
        assert_eq!(manager.snapshot(download).unwrap().status, TaskStatus::Running);
        assert_eq!(manager.current_id(), Some(export));
    }

    #[test]
    fn released_task_can_be_cancelled_cooperatively() {
        let mut manager = BackgroundTaskManager::default();
        let id = queued(&mut manager, "download");
        assert_eq!(manager.start_next(), Some(id));
        assert!(manager.release_current(id));
        let token = manager.cancellation_token(id).unwrap();
        assert_eq!(
            manager.request_cancel(id),
            CancelTaskResult::CancellationRequested
        );
        assert!(token.load(Ordering::Acquire));
        assert_eq!(manager.snapshot(id).unwrap().status, TaskStatus::Cancelling);
    }

    #[test]
    fn detached_visible_task_remains_in_global_snapshots() {
        let mut manager = BackgroundTaskManager::default();
        let detached = queued(&mut manager, "detached");
        assert_eq!(manager.start_next(), Some(detached));
        assert!(manager.release_current(detached));
        assert!(manager.has_global_visible_tasks());
        assert_eq!(manager.global_current_snapshot().unwrap().id, detached);
        assert_eq!(manager.global_snapshots().len(), 1);
        assert_eq!(manager.global_snapshots()[0].id, detached);
    }

    #[test]
    fn global_waiting_count_excludes_the_displayed_queued_task() {
        let mut manager = BackgroundTaskManager::default();
        let first = queued(&mut manager, "first");
        let _second = queued(&mut manager, "second");
        let (primary, waiting) = manager.global_primary_snapshot_and_waiting_count();
        assert_eq!(primary.unwrap().id, first);
        assert_eq!(waiting, 1);
    }

    #[test]
    fn global_waiting_count_includes_all_tasks_after_the_running_task() {
        let mut manager = BackgroundTaskManager::default();
        let first = queued(&mut manager, "first");
        let _second = queued(&mut manager, "second");
        let _third = queued(&mut manager, "third");
        assert_eq!(manager.start_next(), Some(first));
        let (primary, waiting) = manager.global_primary_snapshot_and_waiting_count();
        assert_eq!(primary.unwrap().id, first);
        assert_eq!(waiting, 2);
    }

    #[test]
    fn hidden_tasks_are_excluded_from_global_task_ui() {
        let mut manager = BackgroundTaskManager::default();
        let hidden = queued(&mut manager, "hidden");
        let visible = queued(&mut manager, "visible");
        assert!(manager.set_global_visible(hidden, false));
        assert_eq!(manager.global_queued_count(), 1);
        assert_eq!(manager.start_next(), Some(hidden));
        assert!(manager.global_current_snapshot().is_none());
        assert_eq!(manager.global_snapshots().len(), 1);
        assert_eq!(manager.global_snapshots()[0].id, visible);
        let (primary, waiting) = manager.global_primary_snapshot_and_waiting_count();
        assert_eq!(primary.unwrap().id, visible);
        assert_eq!(waiting, 0);
    }

}
