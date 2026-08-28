use super::AppTab;
use discord_rich_presence::{activity, DiscordIpc, DiscordIpcClient};
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const APPLICATION_ID_ENV: &str = "AURAW_DISCORD_APPLICATION_ID";
const GET_AURAW_URL: &str = env!("CARGO_PKG_REPOSITORY");
const DISCONNECTED_RETRY_INTERVAL: Duration = Duration::from_secs(30);
const CONNECTED_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PresenceContext {
    Browsing,
    Editing { document_id: u64 },
}

#[derive(Clone, Copy, Debug)]
enum PresenceActivity {
    Browsing,
    Editing { started_at: i64 },
}

enum PresenceCommand {
    Set(PresenceActivity),
    Shutdown,
}

pub(super) struct DiscordPresence {
    application_id: Option<String>,
    enabled: bool,
    sender: Option<mpsc::Sender<PresenceCommand>>,
    current_context: Option<PresenceContext>,
    last_non_settings_context: Option<PresenceContext>,
}

impl DiscordPresence {
    pub(super) fn new(enabled: bool) -> Self {
        let application_id = configured_application_id();
        let enabled = enabled && application_id.is_some();
        if enabled {
            log::info!("Discord Rich Presence is enabled");
        }
        Self {
            application_id,
            enabled,
            sender: None,
            current_context: None,
            last_non_settings_context: None,
        }
    }

    pub(super) fn is_configured(&self) -> bool {
        self.application_id.is_some()
    }

    pub(super) fn set_enabled(&mut self, enabled: bool) -> Result<(), String> {
        if enabled && self.application_id.is_none() {
            return Err(format!(
                "Discord Rich Presence is unavailable because this build has no {APPLICATION_ID_ENV}."
            ));
        }
        if self.enabled == enabled {
            return Ok(());
        }

        self.enabled = enabled;
        self.current_context = None;
        if !enabled {
            self.shutdown_worker();
        }
        Ok(())
    }

    pub(super) fn sync(&mut self, tab: AppTab, document_id: Option<u64>) {
        let context = match tab {
            AppTab::Library => Some(PresenceContext::Browsing),
            AppTab::Develop => {
                document_id.map(|document_id| PresenceContext::Editing { document_id })
            }
            AppTab::Settings => self.last_non_settings_context,
        };

        if tab != AppTab::Settings {
            if let Some(context) = context {
                self.last_non_settings_context = Some(context);
            }
        }
        if !self.enabled || context.is_none() || self.current_context == context {
            return;
        }

        let context = context.expect("presence context was checked above");
        let activity = match context {
            PresenceContext::Browsing => PresenceActivity::Browsing,
            PresenceContext::Editing { .. } => PresenceActivity::Editing {
                started_at: unix_timestamp_seconds(),
            },
        };
        if self.send(PresenceCommand::Set(activity)) {
            self.current_context = Some(context);
        }
    }

    pub(super) fn shutdown(&mut self) {
        self.enabled = false;
        self.current_context = None;
        self.shutdown_worker();
    }

    fn send(&mut self, command: PresenceCommand) -> bool {
        if self.sender.is_none() && !self.start_worker() {
            return false;
        }
        let Some(sender) = self.sender.as_ref() else {
            return false;
        };
        if sender.send(command).is_ok() {
            return true;
        }

        self.sender = None;
        false
    }

    fn start_worker(&mut self) -> bool {
        let Some(application_id) = self.application_id.clone() else {
            return false;
        };
        let (sender, receiver) = mpsc::channel();
        match std::thread::Builder::new()
            .name("auraw-discord-presence".to_owned())
            .spawn(move || presence_worker(&application_id, receiver))
        {
            Ok(_) => {
                self.sender = Some(sender);
                true
            }
            Err(error) => {
                log::warn!("could not start Discord Rich Presence worker: {error}");
                false
            }
        }
    }

    fn shutdown_worker(&mut self) {
        if let Some(sender) = self.sender.take() {
            let _ = sender.send(PresenceCommand::Shutdown);
        }
    }
}

impl Drop for DiscordPresence {
    fn drop(&mut self) {
        self.shutdown_worker();
    }
}

fn configured_application_id() -> Option<String> {
    option_env!("AURAW_DISCORD_APPLICATION_ID")
        .map(str::to_owned)
        .or_else(|| std::env::var(APPLICATION_ID_ENV).ok())
        .map(|value| value.trim().to_owned())
        .filter(|value| value.parse::<u64>().is_ok_and(|value| value != 0))
}

fn unix_timestamp_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or_default()
}

fn presence_worker(application_id: &str, receiver: mpsc::Receiver<PresenceCommand>) {
    let mut client = None;
    let mut desired_activity = None;

    loop {
        let timeout = if client.is_some() {
            CONNECTED_REFRESH_INTERVAL
        } else {
            DISCONNECTED_RETRY_INTERVAL
        };
        match receiver.recv_timeout(timeout) {
            Ok(PresenceCommand::Set(activity)) => desired_activity = Some(activity),
            Ok(PresenceCommand::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                clear_and_close(&mut client);
                return;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }

        let Some(activity) = desired_activity else {
            continue;
        };
        if client.is_none() {
            let mut next_client = DiscordIpcClient::new(application_id);
            match next_client.connect() {
                Ok(()) => client = Some(next_client),
                Err(error) => {
                    log::debug!("Discord Rich Presence is waiting for Discord: {error}");
                    continue;
                }
            }
        }

        let result = client
            .as_mut()
            .expect("Discord client was connected above")
            .set_activity(activity_payload(activity));
        if let Err(error) = result {
            log::debug!("Discord Rich Presence update failed; reconnecting: {error}");
            clear_and_close(&mut client);
        }
    }
}

fn activity_payload(presence: PresenceActivity) -> activity::Activity<'static> {
    let payload = activity::Activity::new()
        .activity_type(activity::ActivityType::Playing)
        .buttons(vec![activity::Button::new("Get AuRaw", GET_AURAW_URL)]);
    match presence {
        PresenceActivity::Browsing => payload.details("Browsing RAW Photos"),
        PresenceActivity::Editing { started_at } => payload
            .details("Editing a Picture")
            .timestamps(activity::Timestamps::new().start(started_at)),
    }
}

fn clear_and_close(client: &mut Option<DiscordIpcClient>) {
    if let Some(mut client) = client.take() {
        let _ = client.clear_activity();
        let _ = client.close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disabled_presence() -> DiscordPresence {
        DiscordPresence {
            application_id: None,
            enabled: false,
            sender: None,
            current_context: None,
            last_non_settings_context: None,
        }
    }

    #[test]
    fn settings_preserves_the_last_library_or_editing_context() {
        let mut presence = disabled_presence();
        presence.sync(AppTab::Library, None);
        presence.sync(AppTab::Settings, None);
        assert_eq!(
            presence.last_non_settings_context,
            Some(PresenceContext::Browsing)
        );

        presence.sync(AppTab::Develop, Some(42));
        presence.sync(AppTab::Settings, None);
        assert_eq!(
            presence.last_non_settings_context,
            Some(PresenceContext::Editing { document_id: 42 })
        );
        assert_ne!(
            presence.last_non_settings_context,
            Some(PresenceContext::Editing { document_id: 43 })
        );
    }

    #[test]
    fn payloads_do_not_expose_document_identity() {
        let browsing =
            serde_json::to_string(&activity_payload(PresenceActivity::Browsing)).unwrap();
        let editing = serde_json::to_string(&activity_payload(PresenceActivity::Editing {
            started_at: 123,
        }))
        .unwrap();

        assert!(browsing.contains("Browsing RAW Photos"));
        assert!(editing.contains("Editing a Picture"));
        assert!(editing.contains("\"start\":123"));
        assert!(browsing.contains("Get AuRaw"));
        assert!(editing.contains("Get AuRaw"));
        assert!(browsing.contains(GET_AURAW_URL));
        assert!(editing.contains(GET_AURAW_URL));
        assert!(!browsing.contains("document"));
        assert!(!editing.contains("document"));
    }
}
