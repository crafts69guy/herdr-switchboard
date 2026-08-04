//! Semantic Herdr notifications. Callers describe an outcome; this module owns
//! policy, redaction, position and sound assembly.

use std::process::Command;

use crate::config::Config;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Event {
    CommandDeliveryFailed,
    TermSucceeded,
    KillSucceeded,
    SignalFailed,
    ListenerStale,
}

#[derive(Clone)]
pub struct Notifier {
    enabled: bool,
    position: String,
    sound: String,
}

impl Notifier {
    pub fn new(cfg: &Config) -> Self {
        Self {
            enabled: cfg.common.notifications,
            position: cfg.common.notification_position.clone(),
            sound: cfg.common.notification_sound.clone(),
        }
    }

    pub fn send(&self, event: Event, subject: Option<&str>) {
        let Some(args) = self.args(event, subject) else {
            return;
        };
        let _ = Command::new("herdr").args(args).status();
    }

    pub fn send_message(&self, body: &str, event_sound: &str) {
        if !self.enabled {
            return;
        }
        let sound = if self.sound == "auto" {
            event_sound
        } else {
            self.sound.as_str()
        };
        let mut args = vec![
            "notification",
            "show",
            "Switchboard",
            "--body",
            body,
            "--sound",
            sound,
        ];
        if !self.position.is_empty() {
            args.extend(["--position", self.position.as_str()]);
        }
        let _ = Command::new("herdr").args(args).status();
    }

    fn args(&self, event: Event, subject: Option<&str>) -> Option<Vec<String>> {
        if !self.enabled {
            return None;
        }
        let safe_subject = subject
            .map(redact_subject)
            .filter(|value| !value.is_empty());
        let (body, automatic_sound) = match event {
            Event::CommandDeliveryFailed => (
                "Could not deliver the selected command to its origin pane.".into(),
                "request",
            ),
            Event::TermSucceeded => (
                format!("Sent TERM{}.", suffix(safe_subject.as_deref())),
                "request",
            ),
            Event::KillSucceeded => (
                format!("Sent KILL{}.", suffix(safe_subject.as_deref())),
                "request",
            ),
            Event::SignalFailed => (
                format!(
                    "Could not signal listener{}.",
                    suffix(safe_subject.as_deref())
                ),
                "request",
            ),
            Event::ListenerStale => (
                format!(
                    "Listener{} changed before the action could run.",
                    suffix(safe_subject.as_deref())
                ),
                "request",
            ),
        };
        let sound = if self.sound == "auto" {
            automatic_sound
        } else {
            self.sound.as_str()
        };
        let mut args = vec![
            "notification".into(),
            "show".into(),
            "Switchboard".into(),
            "--body".into(),
            body,
            "--sound".into(),
            sound.into(),
        ];
        if !self.position.is_empty() {
            args.extend(["--position".into(), self.position.clone()]);
        }
        Some(args)
    }
}

pub fn cli(args: &[String], cfg: &Config) -> anyhow::Result<()> {
    let body = args
        .first()
        .map(String::as_str)
        .unwrap_or("Switchboard needs attention.");
    let sound = args.get(1).map(String::as_str).unwrap_or("none");
    anyhow::ensure!(
        matches!(sound, "none" | "done" | "request"),
        "invalid notification sound"
    );
    Notifier::new(cfg).send_message(body, sound);
    Ok(())
}

fn suffix(subject: Option<&str>) -> String {
    subject.map(|value| format!(" {value}")).unwrap_or_default()
}

fn redact_subject(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, ':' | '-' | '_'))
        .take(32)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_failure_never_contains_command_or_secret() {
        let notifier = Notifier::new(&Config::default());
        let args = notifier
            .args(Event::CommandDeliveryFailed, Some("curl token=secret"))
            .unwrap();
        let rendered = args.join(" ");
        assert!(!rendered.contains("curl"));
        assert!(!rendered.contains("secret"));
        assert!(rendered.contains("--position top-right"));
    }

    #[test]
    fn disabled_policy_emits_nothing() {
        let mut cfg = Config::default();
        cfg.common.notifications = false;
        assert!(Notifier::new(&cfg)
            .args(Event::SignalFailed, Some(":3000"))
            .is_none());
    }
}
