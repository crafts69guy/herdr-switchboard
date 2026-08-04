//! Native Port Monitor for local TCP listeners.

use std::collections::{BTreeSet, HashMap};
use std::io::Write;
use std::net::IpAddr;
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, Signal, System, Users};

use crate::config::Config;
use crate::data::Theme;
use crate::notify::{Event as NotifyEvent, Notifier};
use crate::picker::{self, ActionOutcome, ActionSpec, PickerItem, PickerMode};
use crate::query::{Document, FieldSchema, MatchKind};
use crossterm::event::{KeyCode, KeyModifiers};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RawListener {
    pub address: IpAddr,
    pub port: u16,
    pub pid: u32,
    pub process_name: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProcessMeta {
    pub command: String,
    pub cwd: Option<PathBuf>,
    pub parent_pid: Option<u32>,
    pub user: Option<String>,
    pub start_time: u64,
    pub owned_by_current_user: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct PortIdentity {
    pub pid: u32,
    pub port: u16,
    pub start_time: u64,
    pub addresses: Vec<IpAddr>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PortEntry {
    pub identity: PortIdentity,
    pub addresses: Vec<IpAddr>,
    pub process_name: String,
    pub command: String,
    pub cwd: Option<PathBuf>,
    pub parent_pid: Option<u32>,
    pub user: Option<String>,
    pub can_signal: bool,
}

pub trait NativeProbe {
    fn listeners(&mut self) -> Result<Vec<RawListener>>;
    fn process(&mut self, pid: u32) -> Option<ProcessMeta>;
    fn signal(&mut self, pid: u32, signal: PortSignal) -> Result<()>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PortSignal {
    Term,
    Kill,
}

pub struct PortMonitor<P> {
    probe: P,
}

impl<P: NativeProbe> PortMonitor<P> {
    pub fn new(probe: P) -> Self {
        Self { probe }
    }

    pub fn snapshot(&mut self) -> Result<Vec<PortEntry>> {
        let mut groups: HashMap<(u32, u16), (BTreeSet<IpAddr>, String)> = HashMap::new();
        for listener in self.probe.listeners()? {
            let group = groups
                .entry((listener.pid, listener.port))
                .or_insert_with(|| (BTreeSet::new(), listener.process_name));
            group.0.insert(listener.address);
        }
        let mut entries = Vec::with_capacity(groups.len());
        for ((pid, port), (addresses, process_name)) in groups {
            let meta = self.probe.process(pid).unwrap_or_default();
            entries.push(PortEntry {
                identity: PortIdentity {
                    pid,
                    port,
                    start_time: meta.start_time,
                    addresses: addresses.iter().copied().collect(),
                },
                addresses: addresses.into_iter().collect(),
                process_name,
                command: meta.command,
                cwd: meta.cwd,
                parent_pid: meta.parent_pid,
                user: meta.user,
                can_signal: meta.owned_by_current_user,
            });
        }
        entries.sort_by_key(|entry| (entry.identity.port, entry.identity.pid));
        Ok(entries)
    }

    pub fn signal(&mut self, identity: &PortIdentity, signal: PortSignal) -> Result<()> {
        let fresh_addresses = self
            .probe
            .listeners()?
            .into_iter()
            .filter(|listener| listener.pid == identity.pid && listener.port == identity.port)
            .map(|listener| listener.address)
            .collect::<BTreeSet<_>>();
        let expected_addresses = identity.addresses.iter().copied().collect::<BTreeSet<_>>();
        let still_listening = !fresh_addresses.is_empty() && fresh_addresses == expected_addresses;
        let meta = self.probe.process(identity.pid);
        let same_process = meta.as_ref().is_some_and(|meta| {
            meta.start_time == identity.start_time && meta.owned_by_current_user
        });
        anyhow::ensure!(
            still_listening && same_process,
            "listener is stale or no longer signalable"
        );
        self.probe.signal(identity.pid, signal)
    }
}

pub struct SystemProbe {
    system: System,
    users: Users,
    current_user: Option<String>,
}

impl SystemProbe {
    pub fn new() -> Self {
        let system = System::new_all();
        let users = Users::new_with_refreshed_list();
        let current_user = sysinfo::get_current_pid()
            .ok()
            .and_then(|pid| system.process(pid))
            .and_then(|process| process.user_id())
            .and_then(|uid| users.get_user_by_id(uid))
            .map(|user| user.name().to_string());
        Self {
            system,
            users,
            current_user,
        }
    }

    fn refresh(&mut self, pid: u32) {
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[Pid::from_u32(pid)]),
            true,
            ProcessRefreshKind::everything(),
        );
    }
}

impl NativeProbe for SystemProbe {
    fn listeners(&mut self) -> Result<Vec<RawListener>> {
        let listeners = listeners::get_all().map_err(|error| anyhow::anyhow!(error.to_string()))?;
        Ok(listeners
            .into_iter()
            .filter(|listener| {
                listener.protocol == listeners::Protocol::TCP
                    && listener.state == listeners::SocketState::Listen
            })
            .map(|listener| RawListener {
                address: listener.socket.ip(),
                port: listener.socket.port(),
                pid: listener.process.pid,
                process_name: listener.process.name,
            })
            .collect())
    }

    fn process(&mut self, pid: u32) -> Option<ProcessMeta> {
        self.refresh(pid);
        let process = self.system.process(Pid::from_u32(pid))?;
        let user = process
            .user_id()
            .and_then(|uid| self.users.get_user_by_id(uid))
            .map(|user| user.name().to_string());
        Some(ProcessMeta {
            command: process
                .cmd()
                .iter()
                .map(|part| part.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" "),
            cwd: process.cwd().map(PathBuf::from),
            parent_pid: process.parent().map(Pid::as_u32),
            owned_by_current_user: user.is_some() && user == self.current_user,
            user,
            start_time: process.start_time(),
        })
    }

    fn signal(&mut self, pid: u32, signal: PortSignal) -> Result<()> {
        self.refresh(pid);
        let process = self
            .system
            .process(Pid::from_u32(pid))
            .context("process disappeared before signal")?;
        let signal = match signal {
            PortSignal::Term => Signal::Term,
            PortSignal::Kill => Signal::Kill,
        };
        anyhow::ensure!(
            process.kill_with(signal).unwrap_or(false),
            "could not send signal"
        );
        Ok(())
    }
}

pub struct PortWorker {
    rx: Receiver<Result<Vec<PortEntry>, String>>,
    stop: Sender<()>,
    join: Option<JoinHandle<()>>,
}

impl PortWorker {
    pub fn start(interval: Duration) -> Self {
        let (result_tx, rx) = mpsc::channel();
        let (stop, stop_rx) = mpsc::channel();
        let join = thread::spawn(move || {
            let mut monitor = PortMonitor::new(SystemProbe::new());
            loop {
                let result = monitor.snapshot().map_err(|error| error.to_string());
                if result_tx.send(result).is_err() {
                    break;
                }
                if stop_rx.recv_timeout(interval).is_ok() {
                    break;
                }
            }
        });
        Self {
            rx,
            stop,
            join: Some(join),
        }
    }

    pub fn latest(&self) -> Option<Result<Vec<PortEntry>, String>> {
        let mut latest = None;
        while let Ok(snapshot) = self.rx.try_recv() {
            latest = Some(snapshot);
        }
        latest
    }
}

impl Drop for PortWorker {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

pub fn main(cfg: Config, theme: Theme) -> Result<()> {
    let normal = cfg.common.keymode == "normal";
    picker::run(
        PortMode::new(
            cfg.ports.refresh_interval_ms,
            Notifier::new(&cfg),
            cfg.keys.get("ports").cloned().unwrap_or_default(),
        ),
        theme,
        normal,
    )
}

struct PortMode {
    worker: PortWorker,
    entries: Vec<PortEntry>,
    notifier: Notifier,
    bindings: HashMap<String, String>,
}

impl PortMode {
    fn new(
        refresh_interval_ms: u64,
        notifier: Notifier,
        bindings: HashMap<String, String>,
    ) -> Self {
        Self {
            worker: PortWorker::start(Duration::from_millis(refresh_interval_ms)),
            entries: Vec::new(),
            notifier,
            bindings,
        }
    }

    fn items(&self) -> Vec<PickerItem> {
        self.entries.iter().map(port_item).collect()
    }
}

impl PickerMode for PortMode {
    fn title(&self) -> &str {
        "Ports"
    }
    fn accent_slot(&self) -> &'static str {
        "teal"
    }
    fn schema(&self) -> FieldSchema {
        FieldSchema::new(
            &[
                ("port", MatchKind::Exact),
                ("address", MatchKind::Contains),
                ("pid", MatchKind::Exact),
                ("process", MatchKind::Contains),
                ("cwd", MatchKind::Contains),
                ("repo", MatchKind::Contains),
                ("user", MatchKind::Contains),
            ],
            &[("proc", "process")],
        )
    }
    fn actions(&self) -> Vec<ActionSpec> {
        vec![
            ActionSpec {
                id: "copy",
                key: KeyCode::Enter,
                modifiers: KeyModifiers::NONE,
                key_label: "↵",
                label: "copy",
                color_slot: "blue",
            },
            ActionSpec {
                id: "http",
                key: KeyCode::Enter,
                modifiers: KeyModifiers::CONTROL,
                key_label: "^↵",
                label: "http",
                color_slot: "green",
            },
            ActionSpec {
                id: "https",
                key: KeyCode::Enter,
                modifiers: KeyModifiers::ALT,
                key_label: "⌥↵",
                label: "https",
                color_slot: "mauve",
            },
            ActionSpec {
                id: "workspace",
                key: KeyCode::Char('w'),
                modifiers: KeyModifiers::CONTROL,
                key_label: "^w",
                label: "workspace",
                color_slot: "peach",
            },
            ActionSpec {
                id: "term",
                key: KeyCode::Char('x'),
                modifiers: KeyModifiers::CONTROL,
                key_label: "^x",
                label: "term",
                color_slot: "red",
            },
            ActionSpec {
                id: "kill",
                key: KeyCode::Char('x'),
                modifiers: KeyModifiers::ALT,
                key_label: "⌥x",
                label: "force",
                color_slot: "red",
            },
        ]
    }
    fn key_bindings(&self) -> HashMap<String, String> {
        Config::try_load()
            .ok()
            .and_then(|cfg| cfg.keys.get("ports").cloned())
            .unwrap_or_else(|| self.bindings.clone())
    }
    fn action_disabled_reason(&self, item_id: &str, action: &str) -> Option<String> {
        let entry = self
            .entries
            .iter()
            .find(|entry| port_id(entry) == item_id)?;
        match action {
            "workspace" if entry.cwd.as_deref().is_none_or(|cwd| !cwd.is_dir()) => {
                Some("workspace is unavailable because the process cwd is hidden or missing".into())
            }
            "term" | "kill" if !entry.can_signal => Some(
                "signal is disabled because the listener is not owned by the current user".into(),
            ),
            _ => None,
        }
    }
    fn reload_config(&mut self, config: &Config) -> Result<()> {
        self.worker = PortWorker::start(Duration::from_millis(config.ports.refresh_interval_ms));
        self.entries.clear();
        self.notifier = Notifier::new(config);
        self.bindings = config.keys.get("ports").cloned().unwrap_or_default();
        Ok(())
    }
    fn initial(&mut self) -> Result<Vec<PickerItem>> {
        Ok(Vec::new())
    }
    fn poll(&mut self) -> Option<Result<Vec<PickerItem>>> {
        self.worker.latest().map(|result| match result {
            Ok(entries) => {
                self.entries = entries;
                Ok(self.items())
            }
            Err(error) => Err(anyhow::anyhow!(error)),
        })
    }
    fn execute(&mut self, item_id: &str, action: &str) -> Result<ActionOutcome> {
        let entry = self
            .entries
            .iter()
            .find(|entry| port_id(entry) == item_id)
            .cloned()
            .context("listener disappeared")?;
        let endpoint = format!("localhost:{}", entry.identity.port);
        match action {
            "copy" => crate::commands::copy_text(&endpoint)?,
            "http" => open_url(&format!("http://{endpoint}"))?,
            "https" => open_url(&format!("https://{endpoint}"))?,
            "workspace" => {
                let cwd = entry.cwd.as_deref().context("process cwd is unavailable")?;
                anyhow::ensure!(cwd.is_dir(), "process cwd no longer exists");
                let label = cwd
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("port");
                let status = Command::new("herdr")
                    .args(["workspace", "create", "--cwd"])
                    .arg(cwd)
                    .args(["--label", label, "--focus"])
                    .status()?;
                anyhow::ensure!(status.success(), "herdr workspace create failed");
            }
            "term" => {
                confirm_signal(&entry, false)?;
                if let Err(error) =
                    PortMonitor::new(SystemProbe::new()).signal(&entry.identity, PortSignal::Term)
                {
                    let event = if error.to_string().contains("stale") {
                        NotifyEvent::ListenerStale
                    } else {
                        NotifyEvent::SignalFailed
                    };
                    self.notifier.send(event, Some(&endpoint));
                    return Err(error);
                }
                self.notifier
                    .send(NotifyEvent::TermSucceeded, Some(&endpoint));
            }
            "kill" => {
                confirm_signal(&entry, true)?;
                if let Err(error) =
                    PortMonitor::new(SystemProbe::new()).signal(&entry.identity, PortSignal::Kill)
                {
                    let event = if error.to_string().contains("stale") {
                        NotifyEvent::ListenerStale
                    } else {
                        NotifyEvent::SignalFailed
                    };
                    self.notifier.send(event, Some(&endpoint));
                    return Err(error);
                }
                self.notifier
                    .send(NotifyEvent::KillSucceeded, Some(&endpoint));
            }
            _ => anyhow::bail!("unknown port action {action}"),
        }
        Ok(ActionOutcome::Close)
    }
}

fn port_item(entry: &PortEntry) -> PickerItem {
    let addresses = entry
        .addresses
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    let cwd = entry
        .cwd
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    let repo = entry
        .cwd
        .as_ref()
        .and_then(|path| path.file_name())
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_string();
    let user = entry.user.clone().unwrap_or_else(|| "unknown".into());
    let pid = entry.identity.pid.to_string();
    let port = entry.identity.port.to_string();
    let mut preview = vec![
        format!("endpoint  localhost:{port}"),
        format!("address  {addresses}"),
        format!("pid       {pid}"),
        format!("process   {}", entry.process_name),
        format!("command   {}", entry.command),
        format!("user      {user}"),
        format!("started   {}", entry.identity.start_time),
    ];
    if let Some(parent) = entry.parent_pid {
        preview.push(format!("ppid      {parent}"));
    }
    if !cwd.is_empty() {
        preview.push(format!("cwd       {cwd}"));
    }
    if !entry.can_signal {
        preview.push("signal    disabled (not owned by current user)".into());
    }
    PickerItem {
        id: port_id(entry),
        primary: format!(":{port}"),
        secondary: format!("{} · pid {pid}", entry.process_name),
        trailing: None,
        document: Document {
            fuzzy: format!(
                "{port} {addresses} {pid} {} {} {cwd} {repo} {user}",
                entry.process_name, entry.command
            ),
            fields: picker::fields(&[
                ("port", port),
                ("address", addresses),
                ("pid", pid),
                (
                    "process",
                    format!("{} {}", entry.process_name, entry.command),
                ),
                ("cwd", cwd),
                ("repo", repo),
                ("user", user),
            ]),
        },
        preview,
        accent_slot: Some("teal".into()),
    }
}

fn port_id(entry: &PortEntry) -> String {
    let addresses = entry
        .identity
        .addresses
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{}:{}:{}:{}",
        entry.identity.pid, entry.identity.port, entry.identity.start_time, addresses
    )
}

fn open_url(url: &str) -> Result<()> {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    anyhow::ensure!(
        Command::new(program).arg(url).status()?.success(),
        "could not open {url}"
    );
    Ok(())
}

fn confirm_signal(entry: &PortEntry, force: bool) -> Result<()> {
    anyhow::ensure!(entry.can_signal, "listener belongs to another user");
    let word = if force { "kill" } else { "term" };
    println!(
        "\x1b[1m{} process on port {}?\x1b[0m\nPID {}\n{}\n",
        if force { "Force kill" } else { "Stop" },
        entry.identity.port,
        entry.identity.pid,
        entry.command
    );
    print!("Type {word} to confirm: ");
    std::io::stdout().flush()?;
    let mut reply = String::new();
    std::io::stdin().read_line(&mut reply)?;
    anyhow::ensure!(reply.trim() == word, "signal cancelled");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FakeProbe {
        listeners: Vec<RawListener>,
        processes: HashMap<u32, ProcessMeta>,
        signals: Vec<(u32, PortSignal)>,
    }

    impl NativeProbe for FakeProbe {
        fn listeners(&mut self) -> Result<Vec<RawListener>> {
            Ok(self.listeners.clone())
        }
        fn process(&mut self, pid: u32) -> Option<ProcessMeta> {
            self.processes.get(&pid).cloned()
        }
        fn signal(&mut self, pid: u32, signal: PortSignal) -> Result<()> {
            self.signals.push((pid, signal));
            Ok(())
        }
    }

    fn raw(address: &str, port: u16, pid: u32) -> RawListener {
        RawListener {
            address: address.parse().unwrap(),
            port,
            pid,
            process_name: "node".into(),
        }
    }

    #[test]
    fn snapshot_groups_ipv4_and_ipv6_for_same_pid_and_port() {
        let probe = FakeProbe {
            listeners: vec![
                raw("0.0.0.0", 3000, 10),
                raw("::", 3000, 10),
                raw("127.0.0.1", 3000, 11),
            ],
            processes: HashMap::from([
                (
                    10,
                    ProcessMeta {
                        start_time: 5,
                        owned_by_current_user: true,
                        ..Default::default()
                    },
                ),
                (
                    11,
                    ProcessMeta {
                        start_time: 6,
                        owned_by_current_user: true,
                        ..Default::default()
                    },
                ),
            ]),
            signals: Vec::new(),
        };
        let mut monitor = PortMonitor::new(probe);
        let entries = monitor.snapshot().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].addresses.len(), 2);
        assert_eq!(entries[1].identity.pid, 11);
    }

    #[test]
    fn signal_rejects_pid_reuse_before_touching_process() {
        let probe = FakeProbe {
            listeners: vec![raw("127.0.0.1", 3000, 10)],
            processes: HashMap::from([(
                10,
                ProcessMeta {
                    start_time: 99,
                    owned_by_current_user: true,
                    ..Default::default()
                },
            )]),
            signals: Vec::new(),
        };
        let mut monitor = PortMonitor::new(probe);
        let error = monitor
            .signal(
                &PortIdentity {
                    pid: 10,
                    port: 3000,
                    start_time: 5,
                    addresses: vec!["127.0.0.1".parse().unwrap()],
                },
                PortSignal::Term,
            )
            .unwrap_err();
        assert!(error.to_string().contains("stale"));
        assert!(monitor.probe.signals.is_empty());
    }

    #[test]
    fn signal_targets_only_listener_owner_pid() {
        let probe = FakeProbe {
            listeners: vec![raw("127.0.0.1", 3000, 10)],
            processes: HashMap::from([(
                10,
                ProcessMeta {
                    start_time: 5,
                    owned_by_current_user: true,
                    parent_pid: Some(1),
                    ..Default::default()
                },
            )]),
            signals: Vec::new(),
        };
        let mut monitor = PortMonitor::new(probe);
        monitor
            .signal(
                &PortIdentity {
                    pid: 10,
                    port: 3000,
                    start_time: 5,
                    addresses: vec!["127.0.0.1".parse().unwrap()],
                },
                PortSignal::Term,
            )
            .unwrap();
        assert_eq!(monitor.probe.signals, [(10, PortSignal::Term)]);
    }
}
