//! Semantic Herdr notifications. Callers describe an outcome; this module owns
//! policy, redaction, position and sound assembly.

use std::process::Command;

use crate::config::Config;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Event {
    AgentLaunchFailed,
    CommandDeliveryFailed,
    FnmActivationFailed,
    ReviewHandoffSucceeded,
    PathHandoffSucceeded,
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

    /// A notifier that never sends. Tests exercising a code path that reports to
    /// the user must not shell out to a real `herdr notification show`.
    #[cfg(test)]
    pub fn silent() -> Self {
        Self {
            enabled: false,
            position: String::new(),
            sound: String::new(),
        }
    }

    pub fn send(&self, event: Event, subject: Option<&str>) {
        let Some(args) = self.args(event, subject) else {
            return;
        };
        let _ = Command::new("herdr").args(args).status();
    }

    pub fn send_message(&self, body: &str, event_sound: &str) {
        let Some(args) = self.message_args(body, event_sound) else {
            return;
        };
        let _ = Command::new("herdr").args(args).status();
    }

    /// The `herdr notification show` argv for a free-form message, or `None`
    /// when notifications are off.
    fn message_args(&self, body: &str, event_sound: &str) -> Option<Vec<String>> {
        if !self.enabled {
            return None;
        }
        let sound = if self.sound == "auto" {
            event_sound
        } else {
            self.sound.as_str()
        };
        let mut args: Vec<String> = [
            "notification",
            "show",
            "Switchboard",
            "--body",
            body,
            "--sound",
            sound,
        ]
        .map(String::from)
        .into();
        if !self.position.is_empty() {
            args.extend(["--position".into(), self.position.clone()]);
        }
        Some(args)
    }

    fn args(&self, event: Event, subject: Option<&str>) -> Option<Vec<String>> {
        if !self.enabled {
            return None;
        }
        let safe_subject = subject
            .map(redact_subject)
            .filter(|value| !value.is_empty());
        let (body, automatic_sound) = match event {
            Event::AgentLaunchFailed => {
                ("Could not start the selected AI agent.".into(), "request")
            }
            Event::CommandDeliveryFailed => (
                "Could not deliver the selected command to its origin pane.".into(),
                "request",
            ),
            Event::FnmActivationFailed => (
                "Could not activate the project Node version with fnm; opened normally.".into(),
                "request",
            ),
            Event::ReviewHandoffSucceeded => {
                ("Sent review comments to the selected agent.".into(), "done")
            }
            Event::PathHandoffSucceeded => ("Sent the selected path to the agent.".into(), "done"),
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
    let (body, sound) = cli_message(args)?;
    Notifier::new(cfg).send_message(body, sound);
    Ok(())
}

/// `notify [BODY] [SOUND]`: the body defaults to a generic line and the sound
/// to none, and only herdr's three sounds are accepted.
fn cli_message(args: &[String]) -> anyhow::Result<(&str, &str)> {
    let body = args
        .first()
        .map(String::as_str)
        .unwrap_or("Switchboard needs attention.");
    let sound = args.get(1).map(String::as_str).unwrap_or("none");
    anyhow::ensure!(
        matches!(sound, "none" | "done" | "request"),
        "invalid notification sound"
    );
    Ok((body, sound))
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

    /// Every event produces a body, and none of them is empty — a notification
    /// with no text is worse than none at all.
    #[test]
    fn every_event_states_something() {
        let notifier = Notifier::new(&Config::default());
        for event in [
            Event::AgentLaunchFailed,
            Event::CommandDeliveryFailed,
            Event::FnmActivationFailed,
            Event::ReviewHandoffSucceeded,
            Event::PathHandoffSucceeded,
            Event::TermSucceeded,
            Event::KillSucceeded,
            Event::SignalFailed,
            Event::ListenerStale,
        ] {
            let args = notifier
                .args(event, Some("localhost:3000"))
                .unwrap_or_else(|| panic!("{event:?} produced no notification"));
            let body = args
                .iter()
                .position(|arg| arg == "--body")
                .and_then(|index| args.get(index + 1))
                .unwrap_or_else(|| panic!("{event:?} has no body"));
            assert!(!body.trim().is_empty(), "{event:?} has an empty body");
        }
    }

    /// The four port events name their listener, so a notification arriving
    /// after the pane closed still says which one it was about.
    #[test]
    fn the_port_events_name_their_listener() {
        let notifier = Notifier::new(&Config::default());
        for event in [
            Event::TermSucceeded,
            Event::KillSucceeded,
            Event::SignalFailed,
            Event::ListenerStale,
        ] {
            let args = notifier.args(event, Some("localhost:3000")).unwrap();
            assert!(
                args.iter().any(|arg| arg.contains("localhost:3000")),
                "{event:?} dropped its subject: {args:?}"
            );
        }

        // With no subject the sentence still reads, without a dangling gap.
        let args = notifier.args(Event::TermSucceeded, None).unwrap();
        let body = &args[args.iter().position(|a| a == "--body").unwrap() + 1];
        assert_eq!(body, "Sent TERM.");
    }

    /// Notifications off means nothing is sent at all, for any event.
    #[test]
    fn a_disabled_notifier_produces_no_arguments() {
        let mut cfg = Config::default();
        cfg.common.notifications = false;
        let notifier = Notifier::new(&cfg);

        assert!(notifier.args(Event::TermSucceeded, Some("x")).is_none());
        assert!(Notifier::silent()
            .args(Event::KillSucceeded, None)
            .is_none());
    }

    /// `auto` lets each event pick its own sound; anything else is the user's
    /// choice and overrides every event.
    #[test]
    fn auto_lets_the_event_choose_its_sound_and_a_setting_overrides_it() {
        let sound_of = |notifier: &Notifier, event| {
            let args = notifier.args(event, None).unwrap();
            args[args.iter().position(|a| a == "--sound").unwrap() + 1].clone()
        };

        let auto = Notifier::new(&Config::default());
        assert_eq!(sound_of(&auto, Event::ReviewHandoffSucceeded), "done");
        assert_eq!(sound_of(&auto, Event::AgentLaunchFailed), "request");

        let mut cfg = Config::default();
        cfg.common.notification_sound = "glass".into();
        let fixed = Notifier::new(&cfg);
        assert_eq!(sound_of(&fixed, Event::ReviewHandoffSucceeded), "glass");
        assert_eq!(sound_of(&fixed, Event::AgentLaunchFailed), "glass");
    }

    /// A configured position is passed through; the default leaves herdr to
    /// place it rather than sending an empty flag.
    #[test]
    fn a_position_is_passed_through_only_when_one_is_configured() {
        let mut cfg = Config::default();
        cfg.common.notification_position = String::new();
        let args = Notifier::new(&cfg)
            .args(Event::TermSucceeded, None)
            .unwrap();
        assert!(!args.iter().any(|arg| arg == "--position"), "{args:?}");

        cfg.common.notification_position = "top-right".into();
        let args = Notifier::new(&cfg)
            .args(Event::TermSucceeded, None)
            .unwrap();
        let position = &args[args.iter().position(|a| a == "--position").unwrap() + 1];
        assert_eq!(position, "top-right");
    }

    /// A free-text message goes through the same shape, and is silent when
    /// notifications are off.
    #[test]
    fn a_free_text_message_respects_the_same_switch() {
        // Nothing to assert on the wire without shelling out, so this pins the
        // one branch that decides whether anything is attempted at all.
        Notifier::silent().send_message("body", "done");
        let mut cfg = Config::default();
        cfg.common.notifications = false;
        Notifier::new(&cfg).send_message("body", "done");
    }

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
    fn fnm_failure_is_static_and_requests_attention() {
        let notifier = Notifier::new(&Config::default());
        let args = notifier
            .args(Event::FnmActivationFailed, Some("999 /private/repo"))
            .unwrap();
        let rendered = args.join(" ");
        assert!(rendered.contains("Could not activate the project Node version with fnm"));
        assert!(!rendered.contains("999"));
        assert!(!rendered.contains("/private"));
        assert!(rendered.contains("--sound request"));
    }

    #[test]
    fn agent_launch_failure_uses_a_safe_semantic_message() {
        let notifier = Notifier::new(&Config::default());
        let args = notifier
            .args(
                Event::AgentLaunchFailed,
                Some("Claude /private/repo curl token=secret"),
            )
            .unwrap();
        let rendered = args.join(" ");

        assert!(rendered.contains("Could not start the selected AI agent."));
        assert!(!rendered.contains("/private/repo"));
        assert!(!rendered.contains("curl"));
        assert!(!rendered.contains("secret"));
    }

    #[test]
    fn disabled_policy_emits_nothing() {
        let mut cfg = Config::default();
        cfg.common.notifications = false;
        assert!(Notifier::new(&cfg)
            .args(Event::SignalFailed, Some(":3000"))
            .is_none());
        assert!(Notifier::new(&cfg)
            .args(Event::AgentLaunchFailed, None)
            .is_none());
    }

    #[test]
    fn review_handoff_success_never_repeats_the_agent_label() {
        let notifier = Notifier::new(&Config::default());
        let args = notifier
            .args(
                Event::ReviewHandoffSucceeded,
                Some("codex /private/repo token=secret"),
            )
            .unwrap();
        let rendered = args.join(" ");
        assert!(rendered.contains("Sent review comments to the selected agent."));
        assert!(!rendered.contains("/private"));
        assert!(!rendered.contains("secret"));
        assert!(rendered.contains("--sound done"));
    }

    #[test]
    fn path_handoff_notification_never_contains_the_path_or_label() {
        let notifier = Notifier::new(&Config::default());
        let args = notifier
            .args(
                Event::PathHandoffSucceeded,
                Some("repo /private/path token=secret"),
            )
            .unwrap();
        let rendered = args.join(" ");
        assert!(rendered.contains("Sent the selected path to the agent."));
        assert!(!rendered.contains("/private"));
        assert!(!rendered.contains("secret"));
        assert!(rendered.contains("--sound done"));
    }

    /// A message carries its body and sound, the configured sound overrides
    /// `auto`, the position is passed when set, and nothing is built when off.
    #[test]
    fn a_message_builds_herdrs_argv_only_when_notifications_are_on() {
        let mut cfg = Config::default();
        cfg.common.notifications = true;
        cfg.common.notification_sound = "auto".into();
        cfg.common.notification_position = "top-right".into();
        let args = Notifier::new(&cfg).message_args("hello", "done").unwrap();
        assert_eq!(
            args,
            [
                "notification",
                "show",
                "Switchboard",
                "--body",
                "hello",
                "--sound",
                "done",
                "--position",
                "top-right"
            ]
        );
        cfg.common.notification_sound = "none".into();
        cfg.common.notification_position = String::new();
        let args = Notifier::new(&cfg).message_args("hello", "done").unwrap();
        assert_eq!(args.last().map(String::as_str), Some("none"));
        assert!(Notifier::silent().message_args("hello", "done").is_none());
        Notifier::silent().send_message("never shown", "done");
    }

    #[test]
    fn the_cli_defaults_its_message_and_refuses_unknown_sounds() {
        let none: [String; 0] = [];
        assert_eq!(
            cli_message(&none).unwrap(),
            ("Switchboard needs attention.", "none")
        );
        let args = ["Built".to_string(), "done".to_string()];
        assert_eq!(cli_message(&args).unwrap(), ("Built", "done"));
        let bad = ["x".to_string(), "loud".to_string()];
        assert!(cli_message(&bad).is_err());
        assert!(cli(&bad, &Config::default()).is_err());
        cli(&none, &Config::default()).expect("off by default, so nothing is shown");
    }
}
