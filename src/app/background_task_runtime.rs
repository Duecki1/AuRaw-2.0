impl AurawApp {
    fn enqueue_background_action(
        &mut self,
        kind: TaskKind,
        name: impl Into<String>,
        progress: TaskProgress,
        details_open: bool,
        action: BackgroundAction,
    ) -> TaskId {
        let id = self
            .background_tasks
            .enqueue(kind, name, progress, details_open);
        self.background_actions.insert(id, action);
        self.egui_ctx.request_repaint();
        id
    }

    fn enqueue_lens_background_action(
        &mut self,
        request: LensCorrectionTaskRequest,
        name: impl Into<String>,
    ) -> TaskId {
        let (id, obsolete) = self.background_tasks.enqueue_coalesced_lens(
            request.document_id,
            request.generation,
            name,
            TaskProgress::indeterminate("Waiting for earlier background work…"),
            true,
        );
        for obsolete_id in obsolete {
            self.background_actions.remove(&obsolete_id);
        }
        self.background_actions
            .insert(id, BackgroundAction::LensCorrection(request));
        self.egui_ctx.request_repaint();
        id
    }

    fn drive_background_tasks(&mut self, frame: &eframe::Frame) {
        let Some(id) = self.background_tasks.start_next() else {
            return;
        };
        let Some(action) = self.background_actions.remove(&id) else {
            self.fail_background_task(id, "The queued background action was unavailable.");
            return;
        };

        match action {
            BackgroundAction::SingleExport(request) => {
                #[cfg(target_os = "android")]
                let direct_path = crate::android::is_direct_export_path(&request.path)
                    .then(|| request.path.clone());
                if let Err(error) = self.start_export_task(id, request, frame) {
                    #[cfg(target_os = "android")]
                    if let Some(path) = direct_path {
                        crate::android::cancel_direct_export(&self.android_app, &path);
                    }
                    self.notice = Some(format!("Export failed: {error}"));
                    self.fail_background_task(id, error);
                }
            }
            BackgroundAction::LibraryBatchExport {
                jobs,
                settings,
                format,
            } => self.start_library_batch_export_task(id, jobs, settings, format, frame),
            BackgroundAction::LensCorrection(request) => {
                self.start_lens_correction_task(id, request)
            }
            BackgroundAction::SubjectMask(request) => self.start_subject_mask_task(id, request),
            BackgroundAction::ObjectMask(request) => self.start_object_mask_task(id, request),
            }
            BackgroundAction::Inpainting(request) => self.start_inpaint_task(id, request),
            BackgroundAction::LibraryAiMaskRefresh { jobs } => {
                self.start_library_ai_mask_refresh_task(id, jobs, frame)
            }
        }
    }

    fn start_export_task(
        &mut self,
        id: TaskId,
        request: ExportTaskRequest,
        frame: &eframe::Frame,
    ) -> Result<(), String> {
        #[cfg(target_os = "android")]
        {
            let restore_preview = self.library_batch_export.is_none();
            self.suspend_android_preview_for_export(frame, restore_preview)?;
        }

        #[cfg(not(target_os = "android"))]
        let _ = frame;

        let cancellation = self
            .background_tasks
            .cancellation_token(id)
            .ok_or_else(|| "The export cancellation token was unavailable.".to_owned())?;
        let receiver = spawn_export_request(request, cancellation);
        self.export_receiver = Some(receiver);
        self.export_progress = Some((0, 0));
        self.export_task_id = Some(id);
        self.notice = None;
        self.background_tasks
            .update_progress(id, TaskProgress::indeterminate("Preparing tiled export…"));
        Ok(())
    }

    #[cfg(not(target_os = "android"))]
    fn start_library_batch_export_task(
        &mut self,
        id: TaskId,
        jobs: VecDeque<LibraryBatchExportJob>,
        settings: ExportSettings,
        format: ExportFormat,
        frame: &eframe::Frame,
    ) {
        let total = jobs.len();
        let Some(render_state) = frame.wgpu_render_state() else {
            self.fail_background_task(id, "eframe is not running with the wgpu backend.");
            return;
        };
        let Some(cancellation) = self.background_tasks.cancellation_token(id) else {
            self.fail_background_task(id, "Batch export lost its cancellation state.");
            return;
        };
        self.library_batch_export = Some(LibraryBatchExportState {
            pending: VecDeque::new(),
            current: None,
            total,
            completed: 0,
            failures: Vec::new(),
            cancel_requested: false,
            format,
            settings: settings.clone(),
        });
        self.library_batch_export_task_id = Some(id);
        self.library_batch_export_tile_progress = None;
        self.background_tasks.update_progress(
            id,
            TaskProgress::units(
                0,
                total as u64,
                Some("images".to_owned()),
                "Preparing batch export…",
            ),
        );
        self.library_batch_export_receiver = Some(spawn_desktop_library_batch_export(
            render_state.device.clone(),
            render_state.queue.clone(),
            jobs,
            format,
            settings,
            self.camera_profile_mode,
            self.camera_profile_folder.clone(),
            self.last_camera_profile.clone(),
            self.new_image_exposure(),
            self.library.decode_gate(),
            cancellation,
            self.egui_ctx.clone(),
        ));
    }

    #[cfg(target_os = "android")]
    fn start_library_batch_export_task(
        &mut self,
        id: TaskId,
        jobs: VecDeque<LibraryBatchExportJob>,
        settings: ExportSettings,
        format: ExportFormat,
        _frame: &eframe::Frame,
    ) {
        let total = jobs.len();
        self.export_settings = settings.clone();
        self.library_batch_export = Some(LibraryBatchExportState {
            pending: jobs,
            current: None,
            total,
            completed: 0,
            failures: Vec::new(),
            cancel_requested: false,
            format,
            settings,
        });
        self.library_batch_export_task_id = Some(id);
        // The Android export-settings dialog is still painted during the frame
        // that enqueues this task. Open the operation progress window only after
        // the FIFO runner starts it on the following frame, avoiding two centered
        // dialogs being visible at once.
        self.background_tasks.set_details_open(id, true);
        self.background_tasks.update_progress(
            id,
            TaskProgress::units(
                0,
                total as u64,
                Some("images".to_owned()),
                "Preparing batch export…",
            ),
        );
        self.start_next_library_export();
    }

    fn start_subject_mask_task(&mut self, id: TaskId, request: SubjectMaskTaskRequest) {
        let Some(cancellation) = self.background_tasks.cancellation_token(id) else {
            self.fail_background_task(id, "Subject-mask task lost its cancellation state.");
            return;
        };
        self.subject_task_id = Some(id);
        self.subject_job_document_id = request.document_id;
        self.subject_job_generation = request.generation;
        self.subject_download_progress = None;
        self.subject_inferencing = request.model_path.exists() && request.vitmatte_path.exists();
        self.background_tasks
            .set_global_visible(id, !self.subject_inferencing);
        self.subject_receiver = Some(spawn_subject_mask(
            request.model_path,
            request.vitmatte_path,
            request.runtime_path,
            request.runtime_sha256,
            request.source.width,
            request.source.height,
            request.source.rgba.to_vec(),
            cancellation,
        ));
        self.background_tasks.update_progress(
            id,
            TaskProgress::indeterminate(if self.subject_inferencing {
                "Running local subject-mask inference…"
            } else {
                "Preparing model download…"
            }),
        );
        self.egui_ctx.request_repaint();
    }

    fn start_object_mask_task(&mut self, id: TaskId, request: ObjectMaskTaskRequest) {
        let Some(cancellation) = self.background_tasks.cancellation_token(id) else {
            self.fail_background_task(id, "Object-mask task lost its cancellation state.");
            return;
        };
        self.object_task_id = Some(id);
        self.object_job_document_id = request.document_id;
        self.object_job_generation = request.generation;
        self.object_job_target = Some(request.target);
        self.object_pending_target = None;
        self.object_download_progress = None;
        self.object_inferencing = request.encoder_path.exists()
            && request.decoder_path.exists()
            && request.vitmatte_path.exists();
        self.background_tasks
            .set_global_visible(id, !self.object_inferencing);
        self.object_decoder_only = request.request.cache.is_some();
        self.object_receiver = Some(spawn_object_mask(
            request.encoder_path,
            request.decoder_path,
            request.vitmatte_path,
            request.runtime_path,
            request.runtime_sha256,
            request.request,
            cancellation,
        ));
        self.background_tasks.update_progress(
            id,
            TaskProgress::indeterminate(if self.object_inferencing {
                "Running local object-mask inference…"
            } else {
                "Preparing model download…"
            }),
        );
        self.egui_ctx.request_repaint();
    }
        self.background_tasks
            request.model_path,
            request.allow_download,
            request.runtime_path,
            request.runtime_sha256,
            request.source.width,
            request.source.height,
            request.source.rgba.to_vec(),
            request.category,
            cancellation,
        ));
        self.background_tasks.update_progress(
            id,
            } else {
                "Preparing model download…"
            }),
        );
        self.egui_ctx.request_repaint();
    }

    fn start_inpaint_task(&mut self, id: TaskId, request: InpaintTaskRequest) {
        let Some(cancellation) = self.background_tasks.cancellation_token(id) else {
            self.fail_background_task(id, "Inpainting task lost its cancellation state.");
            return;
        };
        self.inpaint_task_id = Some(id);
        self.inpaint_job_document_id = request.document_id;
        self.inpaint_job_generation = request.generation;
        self.inpaint_active_dabs = Some(request.dabs);
        self.inpaint_download_progress = None;
        self.inpaint_inferencing = request.model_path.exists();
        self.background_tasks
            .set_global_visible(id, !self.inpaint_inferencing);
        self.inpaint_receiver = Some(spawn_inpaint(
            request.model_path,
            request.runtime_path,
            request.runtime_sha256,
            request.request,
            cancellation,
        ));
        self.background_tasks.update_progress(
            id,
            TaskProgress::indeterminate(if self.inpaint_inferencing {
                "Running local LaMa inpainting…"
            } else {
                "Preparing model download…"
            }),
        );
        self.egui_ctx.request_repaint();
    }

    fn start_library_ai_mask_refresh_task(
        &mut self,
        id: TaskId,
        jobs: VecDeque<LibraryAiMaskRefreshJob>,
        frame: &eframe::Frame,
    ) {
        let total = jobs.len();
        let mask_total = jobs.iter().map(|job| job.mask_targets).sum();
        self.library_ai_mask_refresh = Some(LibraryAiMaskRefreshState {
            pending: jobs,
            current: None,
            phase: LibraryAiMaskRefreshPhase::Loading,
            total,
            completed: 0,
            mask_total,
            mask_completed: 0,
            failures: Vec::new(),
            cancel_requested: false,
        });
        self.library_ai_mask_refresh_task_id = Some(id);
        self.background_tasks.set_global_visible(id, false);
        self.background_tasks.update_progress(
            id,
            TaskProgress::units(
                0,
                total as u64,
                Some("images".to_owned()),
                "Preparing AI-mask refresh…",
            ),
        );
        self.start_next_library_ai_mask_refresh(frame);
    }

    pub(crate) fn show_global_task_control(&mut self, ui: &mut egui::Ui) {
        if !self.has_global_background_tasks() {
            return;
        }
        let (current, queued) = self.global_primary_task_and_waiting_count();
        let compact = cfg!(target_os = "android") || ui.available_width() < 420.0;
        let response = ui
            .horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if let Some(task) = current.as_ref() {
                    if compact {
                        ui.label(egui_phosphor::regular::ACTIVITY);
                    } else {
                        ui.label(
                            egui::RichText::new(&task.name)
                                .small()
                                .color(ui.visuals().strong_text_color()),
                        );
                    }
                    match task.progress.value.fraction() {
                        Some(fraction) => {
                            let width = if compact { 54.0 } else { 92.0 };
                            ui.add_sized(
                                [width, 16.0],
                                egui::ProgressBar::new(fraction).show_percentage(),
                            );
                        }
                        None => {
                            ui.spinner();
                            if !compact {
                                ui.label(egui::RichText::new(&task.progress.phase).small());
                            }
                        }
                    }
                } else {
                    ui.label(egui::RichText::new("Background tasks").small());
                }
                if queued > 0 {
                    ui.label(
                        egui::RichText::new(format!("+{queued}"))
                            .small()
                            .strong()
                            .background_color(ui.visuals().selection.bg_fill),
                    );
                }
            })
            .response
            .interact(egui::Sense::click())
            .on_hover_text("Show background task queue");

        egui::Popup::menu(&response).show(|ui| {
            ui.set_min_width(if compact { 280.0 } else { 380.0 });
            ui.strong("Background tasks");
            ui.separator();
            let snapshots = self.global_background_task_snapshots();
            for task in snapshots {
                let mut cancel = false;
                let mut dismiss = false;
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        let name = ui.selectable_label(false, &task.name);
                        if name.clicked() {
                            self.set_background_task_details_open(task.id, true);
                        }
                        ui.label(egui::RichText::new(&task.progress.phase).small());
                        if let Some(detail) = &task.progress.detail {
                            ui.label(egui::RichText::new(detail).small().weak());
                        }
                        ui.add_sized(
                            [if compact { 210.0 } else { 300.0 }, 14.0],
                            Self::background_task_progress_widget(&task),
                        );
                        if let Some(error) = &task.error {
                            ui.colored_label(egui::Color32::LIGHT_RED, error);
                        }
                    });
                    if task.status == TaskStatus::Failed {
                        if ui
                            .small_button(egui_phosphor::regular::X)
                            .on_hover_text("Dismiss")
                            .clicked()
                        {
                            dismiss = true;
                        }
                    } else if ui
                        .add_enabled(
                            task.status != TaskStatus::Cancelling,
                            egui::Button::new(egui_phosphor::regular::X).small(),
                        )
                        .on_hover_text("Cancel")
                        .clicked()
                    {
                        cancel = true;
                    }
                });
                if cancel {
                    self.cancel_background_task(task.id);
                }
                if dismiss {
                    self.dismiss_background_task_failure(task.id);
                }
                ui.separator();
            }
        });
    }

    fn background_task_progress_widget(task: &TaskSnapshot) -> egui::ProgressBar {
        match &task.progress.value {
            TaskProgressValue::Indeterminate => egui::ProgressBar::new(0.0).animate(true),
            TaskProgressValue::Fraction(fraction) => {
                egui::ProgressBar::new(*fraction).show_percentage()
            }
            TaskProgressValue::Units {
                completed,
                total,
                unit,
            } => {
                if *total == 0 {
                    egui::ProgressBar::new(0.0).animate(true)
                } else {
                    let fraction = (*completed as f32 / *total as f32).clamp(0.0, 1.0);
                    let text = unit.as_deref().map_or_else(
                        || format!("{completed}/{total}"),
                        |unit| format!("{completed}/{total} {unit}"),
                    );
                    egui::ProgressBar::new(fraction).text(text)
                }
            }
        }
    }

    pub(crate) fn show_background_task_detail_windows(&mut self, ctx: &egui::Context) {
        let snapshots = self
            .background_task_snapshots()
            .into_iter()
            .filter(|task| task.details_open)
            .collect::<Vec<_>>();

        for task in snapshots {
            let has_native_window = match &task.kind {
                TaskKind::SubjectMask { .. } => {
                    self.subject_task_id == Some(task.id) && self.subject_receiver.is_some()
                }
                TaskKind::ObjectMask { .. } => {
                    self.object_task_id == Some(task.id) && self.object_receiver.is_some()
                }
                }
                TaskKind::Inpainting { .. } => {
                    self.inpaint_task_id == Some(task.id) && self.inpaint_receiver.is_some()
                }
                TaskKind::LibraryBatchExport => {
                    self.library_batch_export_task_id == Some(task.id)
                        && self.library_batch_export.is_some()
                }
                TaskKind::LibraryAiMaskRefresh => {
                    self.library_ai_mask_refresh_task_id == Some(task.id)
                        && self.library_ai_mask_refresh.is_some()
                }
                TaskKind::SingleExport | TaskKind::LensCorrection { .. } => false,
            };
            if has_native_window {
                continue;
            }

            let title = match &task.kind {
                TaskKind::SingleExport => "Exporting image",
                TaskKind::LibraryBatchExport => "Exporting images",
                TaskKind::LensCorrection { .. } => "Applying lens correction",
                TaskKind::SubjectMask { .. } => "Preparing subject mask",
                TaskKind::ObjectMask { .. } => "Preparing object mask",
                TaskKind::Inpainting { .. } => "Erasing selection",
                TaskKind::LibraryAiMaskRefresh => "Regenerating AI masks",
            };
            #[cfg(not(target_os = "android"))]
            let mut minimize = false;
            let mut cancel = false;
            let mut dismiss = false;
            crate::ui::responsive_popup(egui::Window::new(title), ctx, 430.0)
                .id(egui::Id::new(("background-task-detail", task.id.get())))
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.label(egui::RichText::new(&task.name).strong());
                    ui.label(&task.progress.phase);
                    if let Some(detail) = &task.progress.detail {
                        ui.label(egui::RichText::new(detail).small());
                    }
                    ui.add_space(6.0);
                    ui.add(Self::background_task_progress_widget(&task));
                    if task.status == TaskStatus::Cancelling {
                        ui.label("Stopping at the next safe point…");
                    }
                    if let Some(error) = &task.error {
                        ui.add_space(6.0);
                        ui.colored_label(egui::Color32::LIGHT_RED, error);
                    }
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        #[cfg(not(target_os = "android"))]
                        if ui.button("Minimize").clicked() {
                            minimize = true;
                        }
                        if task.status == TaskStatus::Failed {
                            if ui.button("Dismiss").clicked() {
                                dismiss = true;
                            }
                        } else if ui
                            .add_enabled(
                                task.status != TaskStatus::Cancelling,
                                egui::Button::new("Cancel"),
                            )
                            .clicked()
                        {
                            cancel = true;
                        }
                    });
                });
            #[cfg(not(target_os = "android"))]
            if minimize {
                self.set_background_task_details_open(task.id, false);
            }
            if cancel {
                self.cancel_background_task(task.id);
            }
            if dismiss {
                self.dismiss_background_task_failure(task.id);
            }
        }
    }

    pub(crate) fn library_batch_export_progress_open(&self) -> bool {
        self.library_batch_export_task_id
            .is_some_and(|id| self.background_task_details_open(id))
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn minimize_library_batch_export_progress(&mut self) {
        if let Some(id) = self.library_batch_export_task_id {
            self.set_background_task_details_open(id, false);
        }
    }

    pub(crate) fn library_ai_mask_refresh_progress_open(&self) -> bool {
        self.library_ai_mask_refresh_task_id
            .is_some_and(|id| self.background_task_details_open(id))
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn minimize_library_ai_mask_refresh_progress(&mut self) {
        if let Some(id) = self.library_ai_mask_refresh_task_id {
            self.set_background_task_details_open(id, false);
        }
    }

    pub(crate) fn cancel_library_ai_mask_refresh(&mut self) {
        if let Some(id) = self.library_ai_mask_refresh_task_id {
            self.cancel_background_task(id);
        }
    }

    pub(crate) fn background_task_snapshots(&self) -> Vec<TaskSnapshot> {
        self.background_tasks.snapshots()
    }

    pub(crate) fn global_background_task_snapshots(&self) -> Vec<TaskSnapshot> {
        self.background_tasks.global_snapshots()
    }

    fn global_primary_task_and_waiting_count(&self) -> (Option<TaskSnapshot>, usize) {
        self.background_tasks
            .global_primary_snapshot_and_waiting_count()
    }

    pub(crate) fn has_background_tasks(&self) -> bool {
        self.background_tasks.has_visible_tasks()
    }

    #[cfg(target_os = "android")]
    pub(crate) fn sync_android_task_notification(&self) {
        let (task, waiting_count) = self.global_primary_task_and_waiting_count();

        let Some(task) = task else {
            if let Err(error) =
                crate::android::clear_background_task_notification(&self.android_app)
            {
                log::warn!("{error}");
            }
            return;
        };

        let (progress_percent, indeterminate) = match &task.progress.value {
            TaskProgressValue::Indeterminate => (0, true),
            TaskProgressValue::Fraction(fraction) => {
                (((fraction.clamp(0.0, 1.0) * 100.0).round()) as i32, false)
            }
            TaskProgressValue::Units {
                completed, total, ..
            } => {
                if *total == 0 {
                    (0, true)
                } else {
                    let fraction = (*completed as f64 / *total as f64).clamp(0.0, 1.0);
                    ((fraction * 100.0).round() as i32, false)
                }
            }
        };
        if let Err(error) = crate::android::update_background_task_notification(
            &self.android_app,
            &task.name,
            &task.progress.phase,
            task.progress.detail.as_deref(),
            progress_percent,
            indeterminate,
            waiting_count,
        ) {
            log::warn!("{error}");
        }
    }

    #[cfg(target_os = "android")]
    pub(crate) fn android_foreground_task_active(&self) -> bool {
        // Android intentionally treats every queued, running, cancelling, or
        // unacknowledged failed task as modal foreground work. This prevents a
        // second RAW/preview pipeline from being allocated while export or
        // another long operation owns the device's limited GPU budget.
        self.background_tasks.has_visible_tasks()
    }

    pub(crate) fn has_global_background_tasks(&self) -> bool {
        self.background_tasks.has_global_visible_tasks()
    }

    pub(crate) fn set_background_task_details_open(&mut self, id: TaskId, open: bool) {
        self.background_tasks.set_details_open(id, open);
        self.egui_ctx.request_repaint();
    }

    pub(crate) fn background_task_details_open(&self, id: TaskId) -> bool {
        self.background_tasks.details_open(id)
    }

    pub(crate) fn cancel_background_task(&mut self, id: TaskId) {
        let task_kind = self.background_tasks.snapshot(id).map(|task| task.kind);
        let result = self.background_tasks.request_cancel(id);
        match result {
            CancelTaskResult::RemovedQueued => {
                if let Some(action) = self.background_actions.remove(&id) {
                    match &action {
                        BackgroundAction::SingleExport(request) => {
                            if self.export_task_id == Some(id) {
                                self.export_task_id = None;
                            }
                            #[cfg(target_os = "android")]
                            if crate::android::is_direct_export_path(&request.path) {
                                crate::android::cancel_direct_export(&self.android_app, &request.path);
                            }
                        }
                        BackgroundAction::LibraryBatchExport { .. } => {
                            if self.library_batch_export_task_id == Some(id) {
                                self.library_batch_export_task_id = None;
                            }
                        }
                        BackgroundAction::LensCorrection(request) => {
                            if request.document_id == self.sidecar_generation
                                && request.generation == self.lens_correction_generation
                            {
                                self.lens_correction.enabled = self.lens_correction.applied;
                                self.lens_correction.catalog.status =
                                    "Lens correction change cancelled; previous preview retained."
                                        .to_owned();
                            }
                        }
                        BackgroundAction::SubjectMask(_) => {
                            if self.subject_task_id == Some(id) {
                                self.subject_task_id = None;
                            }
                        }
                        BackgroundAction::ObjectMask(_) => {
                            if self.object_task_id == Some(id) {
                                self.object_task_id = None;
                            }
                        }
                            }
                        }
                        BackgroundAction::Inpainting(_) => {
                            if self.inpaint_task_id == Some(id) {
                                self.inpaint_task_id = None;
                                self.inpaint_active_dabs = None;
                            }
                        }
                        BackgroundAction::LibraryAiMaskRefresh { .. } => {
                            if self.library_ai_mask_refresh_task_id == Some(id) {
                                self.library_ai_mask_refresh_task_id = None;
                            }
                        }
                    }
                }
            }
            CancelTaskResult::CancellationRequested => {
                if matches!(task_kind, Some(TaskKind::LensCorrection { .. })) {
                    self.lens_correction.enabled = self.lens_correction.applied;
                    self.lens_correction.catalog.status =
                        "Cancelling lens correction; previous preview will be retained.".to_owned();
                }
                if self.library_batch_export_task_id == Some(id) {
                    self.cancel_library_batch_export();
                }
                if self.library_ai_mask_refresh_task_id == Some(id) {
                    let finish_now = if let Some(state) = self.library_ai_mask_refresh.as_mut() {
                        state.cancel_requested = true;
                        state.pending.clear();
                        state.current.is_none()
                    } else {
                        false
                    };
                    if self.ai_mask_update_active {
                        self.cancel_ai_mask_update();
                    }
                    if finish_now {
                        self.finish_library_ai_mask_refresh();
                    }
                }
            }
            CancelTaskResult::DismissedFailure | CancelTaskResult::NotFound => {}
        }
        self.egui_ctx.request_repaint();
    }

    pub(crate) fn dismiss_background_task_failure(&mut self, id: TaskId) {
        self.background_tasks.dismiss_failure(id);
    }

    fn background_task_cancelled(&self, id: TaskId) -> bool {
        self.background_tasks.cancellation_requested(id)
    }

    fn finish_background_task(&mut self, id: TaskId) {
        self.background_tasks.complete(id);
        self.egui_ctx.request_repaint();
    }

    fn fail_background_task(&mut self, id: TaskId, error: impl Into<String>) {
        self.background_tasks.fail(id, error);
        self.egui_ctx.request_repaint();
    }

    fn update_background_progress(&mut self, id: Option<TaskId>, progress: TaskProgress) {
        if let Some(id) = id {
            self.background_tasks.update_progress(id, progress);
        }
    }

    fn sync_library_batch_background_progress(&mut self) {
        let Some(id) = self.library_batch_export_task_id else {
            return;
        };
        let Some(batch) = self.library_batch_export.as_ref() else {
            return;
        };
        let current_name = batch.current.as_ref().map(|job| {
            #[cfg(not(target_os = "android"))]
            {
                job.source
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("image")
                    .to_owned()
            }
            #[cfg(target_os = "android")]
            {
                job.display_name.clone()
            }
        });
        let tile_progress = self.library_batch_export_tile_progress();
        let phase = if batch.cancel_requested {
            "Cancelling after the current image finishes…".to_owned()
        } else if let Some(name) = current_name.as_ref() {
            match tile_progress {
                Some((tiles, total_tiles)) if total_tiles > 0 && tiles >= total_tiles => {
                    format!("Finalizing {name}")
                }
                Some((_, total_tiles)) if total_tiles > 0 => format!("Exporting {name}"),
                _ => format!("Preparing {name}"),
            }
        } else {
            "Preparing next image…".to_owned()
        };
        let overall_fraction = batch_export_overall_fraction(
            batch.completed,
            batch.total,
            batch.current.is_some(),
            tile_progress,
        );
        let mut progress = TaskProgress::fraction(overall_fraction, phase);
        let mut details = vec![format!("{}/{} images", batch.completed, batch.total)];
        if let Some(name) = current_name {
            details.push(match tile_progress {
                Some((tiles, total_tiles)) if total_tiles > 0 => {
                    format!("{name}: tile {tiles}/{total_tiles}")
                }
                _ => format!("{name}: preparing tiled render"),
            });
        }
        progress.detail = Some(details.join(" · "));
        self.background_tasks.update_progress(id, progress);
    }

    fn sync_library_ai_mask_background_progress(&mut self) {
        let Some(id) = self.library_ai_mask_refresh_task_id else {
            return;
        };
        let Some(state) = self.library_ai_mask_refresh.as_ref() else {
            return;
        };
        let worker_is_reporting_progress = state.phase == LibraryAiMaskRefreshPhase::Updating
            && ((self.subject_task_id == Some(id) && self.subject_receiver.is_some())
                || (self.object_task_id == Some(id) && self.object_receiver.is_some())
        if worker_is_reporting_progress {
            return;
        }
        let current_name = state.current.as_ref().map(|job| {
            #[cfg(not(target_os = "android"))]
            {
                job.source
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("image")
                    .to_owned()
            }
            #[cfg(target_os = "android")]
            {
                job.display_name.clone()
            }
        });
        let phase = match state.phase {
            LibraryAiMaskRefreshPhase::Loading => "Opening image",
            LibraryAiMaskRefreshPhase::Updating => "Generating AI masks",
            LibraryAiMaskRefreshPhase::Saving => "Saving sidecar",
        };
        let current_mask_progress = state.current.as_ref().map_or(0, |job| {
            if state.phase == LibraryAiMaskRefreshPhase::Loading {
                return 0;
            }
            job.mask_targets
                .saturating_sub(self.ai_mask_update_remaining_target_count())
        });
        let mut progress = TaskProgress::units(
            state.completed as u64,
            state.total as u64,
            Some("images".to_owned()),
            current_name
                .as_ref()
                .map_or_else(|| phase.to_owned(), |name| format!("{phase}: {name}")),
        );
        if state.mask_total > 0 {
            progress.detail = Some(format!(
                "Images {}/{} · Masks {}/{}",
                state.completed,
                state.total,
                state.mask_completed + current_mask_progress,
                state.mask_total
            ));
        }
        self.background_tasks.update_progress(id, progress);
    }

    fn cancel_document_bound_background_tasks(&mut self) {
        let ids = self
            .background_task_snapshots()
            .into_iter()
            .filter(|task| {
                task.status != TaskStatus::Failed
                    && matches!(
                        task.kind,
                        TaskKind::LensCorrection { .. }
                            | TaskKind::SubjectMask { .. }
                            | TaskKind::ObjectMask { .. }
                            | TaskKind::Inpainting { .. }
                    )
            })
            .map(|task| task.id)
            .collect::<Vec<_>>();
        for id in ids {
            self.cancel_background_task(id);
        }
    }

    fn cancel_stale_document_background_tasks(&mut self) {
        let current_document = self.sidecar_generation;
        let stale = self
            .background_task_snapshots()
            .into_iter()
            .filter(|task| {
                task.status != TaskStatus::Failed
                    && match task.kind {
                        TaskKind::LensCorrection { document_id, .. }
                        | TaskKind::SubjectMask { document_id, .. }
                        | TaskKind::ObjectMask { document_id, .. }
                        | TaskKind::Inpainting { document_id, .. } => {
                            document_id != current_document
                        }
                        _ => false,
                    }
            })
            .map(|task| task.id)
            .collect::<Vec<_>>();
        for id in stale {
            self.cancel_background_task(id);
        }
    }
}
