use super::*;

const GITHUB_LATEST_RELEASE_API: &str =
    "https://api.github.com/repos/Duecki1/CalibRaw/releases/latest";
const GITHUB_RELEASE_URL_PREFIX: &str = "https://github.com/Duecki1/CalibRaw/releases/";
const MAX_RELEASE_RESPONSE_BYTES: u64 = 64 * 1024;

#[derive(Clone, Debug)]
struct AvailableUpdate {
    version: semver::Version,
    tag: String,
    name: Option<String>,
    url: String,
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    name: Option<String>,
    html_url: String,
}

#[derive(Clone, Debug)]
enum VersionCheckStatus {
    NotChecked,
    Checking,
    NoPublishedRelease,
    UpToDate,
    Available {
        version: semver::Version,
        ignored: bool,
    },
    Failed(String),
}

pub(in crate::app) struct VersionCheckState {
    receiver: Option<mpsc::Receiver<Result<Option<AvailableUpdate>, String>>>,
    requested_manually: bool,
    dialog: Option<AvailableUpdate>,
    status: VersionCheckStatus,
}

impl Default for VersionCheckState {
    fn default() -> Self {
        Self {
            receiver: None,
            requested_manually: false,
            dialog: None,
            status: VersionCheckStatus::NotChecked,
        }
    }
}

fn normalized_version(tag: &str) -> Result<semver::Version, String> {
    let trimmed = tag.trim();
    let version = trimmed
        .strip_prefix('v')
        .or_else(|| trimmed.strip_prefix('V'))
        .unwrap_or(trimmed);
    semver::Version::parse(version)
        .map_err(|error| format!("GitHub returned an invalid release version {tag:?}: {error}"))
}

const fn should_show_update(requested_manually: bool, auto_check: bool, ignored: bool) -> bool {
    !ignored && (requested_manually || auto_check)
}

fn fetch_latest_release() -> Result<Option<AvailableUpdate>, String> {
    let config = ureq::Agent::config_builder()
        .https_only(true)
        .timeout_global(Some(Duration::from_secs(12)))
        .build();
    let agent: ureq::Agent = config.into();
    let response = agent
        .get(GITHUB_LATEST_RELEASE_API)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2026-03-10")
        .header(
            "User-Agent",
            concat!("CalibRaw/", env!("CARGO_PKG_VERSION")),
        )
        .call();
    let mut response = match response {
        Ok(response) => response,
        Err(ureq::Error::StatusCode(404)) => return Ok(None),
        Err(error) => return Err(format!("GitHub update request failed: {error}")),
    };
    let body = response
        .body_mut()
        .with_config()
        .limit(MAX_RELEASE_RESPONSE_BYTES)
        .read_to_string()
        .map_err(|error| format!("Could not read GitHub's update response: {error}"))?;
    let release: GithubRelease = serde_json::from_str(&body)
        .map_err(|error| format!("Could not understand GitHub's update response: {error}"))?;
    if !release.html_url.starts_with(GITHUB_RELEASE_URL_PREFIX) {
        return Err("GitHub returned an unexpected release URL".to_owned());
    }
    let version = normalized_version(&release.tag_name)?;
    Ok(Some(AvailableUpdate {
        version,
        tag: release.tag_name,
        name: release.name,
        url: release.html_url,
    }))
}

impl CalibRawApp {
    pub(crate) fn check_for_updates(&mut self, requested_manually: bool) {
        if self.ui.version_check.receiver.is_some() {
            return;
        }

        let (sender, receiver) = mpsc::channel();
        let repaint = self.egui_ctx.clone();
        match std::thread::Builder::new()
            .name("calibraw-version-check".to_owned())
            .spawn(move || {
                let result = fetch_latest_release();
                let _ = sender.send(result);
                repaint.request_repaint();
            }) {
            Ok(_) => {
                self.ui.version_check.receiver = Some(receiver);
                self.ui.version_check.requested_manually = requested_manually;
                self.ui.version_check.status = VersionCheckStatus::Checking;
            }
            Err(error) => {
                self.ui.version_check.status = VersionCheckStatus::Failed(format!(
                    "Could not start the update checker: {error}"
                ));
            }
        }
    }

    pub(crate) fn set_auto_check_updates(&mut self, enabled: bool) {
        if self.preferences.auto_check_updates == enabled {
            return;
        }
        self.preferences.auto_check_updates = enabled;
        self.persist_performance_settings();
        if enabled {
            self.check_for_updates(false);
        }
    }

    pub(crate) fn version_check_in_progress(&self) -> bool {
        self.ui.version_check.receiver.is_some()
    }

    pub(crate) fn version_check_status_text(&self) -> String {
        match &self.ui.version_check.status {
            VersionCheckStatus::NotChecked => "Not checked during this session.".to_owned(),
            VersionCheckStatus::Checking => "Checking GitHub…".to_owned(),
            VersionCheckStatus::NoPublishedRelease => {
                "No published GitHub release is available yet.".to_owned()
            }
            VersionCheckStatus::UpToDate => {
                format!("CalibRaw {} is up to date.", env!("CARGO_PKG_VERSION"))
            }
            VersionCheckStatus::Available { version, ignored } => {
                if *ignored {
                    format!("Version {version} is available and ignored.")
                } else {
                    format!("Version {version} is available.")
                }
            }
            VersionCheckStatus::Failed(error) => error.clone(),
        }
    }

    pub(in crate::app) fn poll_version_check(&mut self) {
        let update = self
            .ui
            .version_check
            .receiver
            .as_ref()
            .map(mpsc::Receiver::try_recv);
        let result = match update {
            Some(Ok(result)) => result,
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                Err("The GitHub update checker stopped unexpectedly.".to_owned())
            }
            Some(Err(mpsc::TryRecvError::Empty)) | None => return,
        };
        self.ui.version_check.receiver = None;

        match result {
            Ok(None) => {
                self.ui.version_check.status = VersionCheckStatus::NoPublishedRelease;
            }
            Ok(Some(release)) => {
                let current = match semver::Version::parse(env!("CARGO_PKG_VERSION")) {
                    Ok(version) => version,
                    Err(error) => {
                        self.ui.version_check.status = VersionCheckStatus::Failed(format!(
                            "This build has an invalid version: {error}"
                        ));
                        return;
                    }
                };
                if release.version <= current {
                    self.ui.version_check.status = VersionCheckStatus::UpToDate;
                    return;
                }

                let ignored = self.preferences.ignored_update_version.as_deref()
                    == Some(release.version.to_string().as_str());
                self.ui.version_check.status = VersionCheckStatus::Available {
                    version: release.version.clone(),
                    ignored,
                };
                let should_open = should_show_update(
                    self.ui.version_check.requested_manually,
                    self.preferences.auto_check_updates,
                    ignored,
                );
                if should_open {
                    self.ui.version_check.dialog = Some(release);
                }
            }
            Err(error) => {
                crate::diagnostics::record(format!("Version check failed: {error}"));
                self.ui.version_check.status = VersionCheckStatus::Failed(error);
            }
        }
    }

    pub(in crate::app) fn show_version_update_dialog(&mut self, ctx: &egui::Context) {
        if self.ui.onboarding_step.is_some() {
            return;
        }
        let Some(release) = self.ui.version_check.dialog.clone() else {
            return;
        };

        enum Action {
            Ignore,
            Remind,
            Update,
        }
        let mut action = None;
        crate::ui::responsive_popup(egui::Window::new("CalibRaw update available"), ctx, 480.0)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label(format!(
                    "CalibRaw {} is available. You are using version {}.",
                    release.version,
                    env!("CARGO_PKG_VERSION")
                ));
                if let Some(name) = release
                    .name
                    .as_deref()
                    .filter(|name| *name != release.tag)
                {
                    ui.add_space(4.0);
                    ui.strong(name);
                }
                ui.add_space(6.0);
                ui.small(
                    "Close ignores this version permanently. Remind me next time shows it again after the next app start.",
                );
                ui.add_space(10.0);
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Close").clicked() {
                        action = Some(Action::Ignore);
                    }
                    if ui.button("Remind me next time").clicked() {
                        action = Some(Action::Remind);
                    }
                    if ui.button("Update Now").clicked() {
                        action = Some(Action::Update);
                    }
                });
            });

        match action {
            Some(Action::Ignore) => {
                self.preferences.ignored_update_version = Some(release.version.to_string());
                self.ui.version_check.status = VersionCheckStatus::Available {
                    version: release.version,
                    ignored: true,
                };
                self.ui.version_check.dialog = None;
                self.persist_performance_settings();
            }
            Some(Action::Remind) => self.ui.version_check.dialog = None,
            Some(Action::Update) => {
                ctx.open_url(egui::OpenUrl::new_tab(&release.url));
                self.ui.version_check.dialog = None;
            }
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_style_tags_are_normalized() {
        assert_eq!(normalized_version("v2.4.1").unwrap().to_string(), "2.4.1");
        assert_eq!(normalized_version("2.4.1").unwrap().to_string(), "2.4.1");
    }

    #[test]
    fn semantic_versions_do_not_use_lexicographic_order() {
        assert!(normalized_version("v2.10.0").unwrap() > normalized_version("2.9.0").unwrap());
    }

    #[test]
    fn invalid_release_tags_are_rejected() {
        assert!(normalized_version("latest").is_err());
    }

    #[test]
    fn ignored_version_never_reopens_its_popup() {
        assert!(!should_show_update(false, true, true));
        assert!(!should_show_update(true, true, true));
    }

    #[test]
    fn manual_checks_work_when_automatic_checks_are_disabled() {
        assert!(should_show_update(true, false, false));
        assert!(!should_show_update(false, false, false));
    }
}
