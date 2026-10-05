//! Native Port Monitor for local TCP listeners.

use std::collections::{BTreeSet, HashMap};
use std::io::{BufRead, Write};
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
use crate::runner::{CommandRunner, SystemRunner};
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

    /// A worker that answers from a fixed script and starts no thread.
    ///
    /// The live worker's first act is `SystemProbe::new`, which enumerates every
    /// process on the machine — so a test that only wants to see what `PortMode`
    /// does with a snapshot would otherwise pay for a real system scan, and read
    /// whatever happened to be listening while it ran.
    #[cfg(test)]
    fn seeded(snapshots: Vec<Result<Vec<PortEntry>, String>>) -> Self {
        let (result_tx, rx) = mpsc::channel();
        for snapshot in snapshots {
            result_tx.send(snapshot).expect("seeded receiver is alive");
        }
        let (stop, _) = mpsc::channel();
        Self {
            rx,
            stop,
            join: None,
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
    let mode = PortMode::new(
        cfg.ports.refresh_interval_ms,
        Notifier::new(&cfg),
        cfg.keys.get("ports").cloned().unwrap_or_default(),
    );
    picker::run(mode, theme, cfg)
}

struct PortMode {
    worker: PortWorker,
    entries: Vec<PortEntry>,
    notifier: Notifier,
    bindings: HashMap<String, String>,
    /// The effect edge, swapped for doubles in tests: herdr, the clipboard, the
    /// browser, the typed confirmation, and the revalidated signal.
    runner: Box<dyn CommandRunner>,
    copy: fn(&str) -> Result<()>,
    open: fn(&str) -> Result<()>,
    confirm: fn(&PortEntry, bool) -> Result<()>,
    signal: fn(&PortIdentity, PortSignal) -> Result<()>,
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
            runner: Box::new(SystemRunner),
            copy: crate::clipboard::copy_text,
            open: open_url,
            confirm: confirm_signal,
            signal: system_signal,
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
                key_label: "↵".into(),
                label: "copy",
                color_slot: "blue",
            },
            ActionSpec {
                id: "http",
                key: KeyCode::Enter,
                modifiers: KeyModifiers::CONTROL,
                key_label: "^↵".into(),
                label: "http",
                color_slot: "green",
            },
            ActionSpec {
                id: "https",
                key: KeyCode::Enter,
                modifiers: KeyModifiers::ALT,
                key_label: "⌥↵".into(),
                label: "https",
                color_slot: "mauve",
            },
            ActionSpec {
                id: "workspace",
                key: KeyCode::Char('w'),
                modifiers: KeyModifiers::CONTROL,
                key_label: "^w".into(),
                label: "workspace",
                color_slot: "peach",
            },
            ActionSpec {
                id: "term",
                key: KeyCode::Char('x'),
                modifiers: KeyModifiers::CONTROL,
                key_label: "^x".into(),
                label: "term",
                color_slot: "red",
            },
            ActionSpec {
                id: "kill",
                key: KeyCode::Char('x'),
                modifiers: KeyModifiers::ALT,
                key_label: "⌥x".into(),
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
    /// The refresh worker runs for the whole life of the pane, so an answer can
    /// land at any moment.
    fn is_polling(&self) -> bool {
        true
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
            "copy" => (self.copy)(&endpoint)?,
            "http" => (self.open)(&format!("http://{endpoint}"))?,
            "https" => (self.open)(&format!("https://{endpoint}"))?,
            "workspace" => {
                let cwd = entry.cwd.as_deref().context("process cwd is unavailable")?;
                anyhow::ensure!(cwd.is_dir(), "process cwd no longer exists");
                let label = cwd
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("port");
                let cwd = cwd.to_string_lossy();
                let status = self.runner.status(
                    "herdr",
                    &[
                        "workspace",
                        "create",
                        "--cwd",
                        &cwd,
                        "--label",
                        label,
                        "--focus",
                    ],
                )?;
                anyhow::ensure!(status.success(), "herdr workspace create failed");
            }
            "term" => {
                (self.confirm)(&entry, false)?;
                if let Err(error) = (self.signal)(&entry.identity, PortSignal::Term) {
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
                (self.confirm)(&entry, true)?;
                if let Err(error) = (self.signal)(&entry.identity, PortSignal::Kill) {
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
        trailing_marker: None,
        document: Document::new(
            format!(
                "{port} {addresses} {pid} {} {} {cwd} {repo} {user}",
                entry.process_name, entry.command
            ),
            &[
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
            ],
        ),
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

/// Signal a listener through the live probe, which revalidates it first.
fn system_signal(identity: &PortIdentity, signal: PortSignal) -> Result<()> {
    PortMonitor::new(SystemProbe::new()).signal(identity, signal)
}

fn confirm_signal(entry: &PortEntry, force: bool) -> Result<()> {
    confirm_signal_with(
        entry,
        force,
        &mut std::io::stdin().lock(),
        &mut std::io::stdout(),
    )
}

/// The typed confirmation, over any reader and writer.
fn confirm_signal_with(
    entry: &PortEntry,
    force: bool,
    input: &mut dyn BufRead,
    output: &mut dyn Write,
) -> Result<()> {
    anyhow::ensure!(entry.can_signal, "listener belongs to another user");
    let word = if force { "kill" } else { "term" };
    write!(
        output,
        "\x1b[1m{} process on port {}?\x1b[0m\nPID {}\n{}\n\nType {word} to confirm: ",
        if force { "Force kill" } else { "Stop" },
        entry.identity.port,
        entry.identity.pid,
        entry.command
    )?;
    output.flush()?;
    let mut reply = String::new();
    input.read_line(&mut reply)?;
    anyhow::ensure!(reply.trim() == word, "signal cancelled");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::MockRunner;

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

    /// A listener entry with everything the item builder reads.
    fn entry(port: u16, pid: u32, cwd: Option<&str>, can_signal: bool) -> PortEntry {
        PortEntry {
            identity: PortIdentity {
                pid,
                port,
                start_time: 42,
                addresses: vec!["127.0.0.1".parse().unwrap()],
            },
            addresses: vec!["127.0.0.1".parse().unwrap(), "::1".parse().unwrap()],
            process_name: "node".into(),
            command: "node server.js".into(),
            cwd: cwd.map(PathBuf::from),
            parent_pid: Some(pid - 1),
            user: Some("dev".into()),
            can_signal,
        }
    }

    /// A mode holding `entries`, with a worker that starts no thread.
    fn mode(entries: Vec<PortEntry>) -> PortMode {
        PortMode {
            worker: PortWorker::seeded(Vec::new()),
            entries,
            notifier: Notifier::silent(),
            bindings: HashMap::new(),
            runner: Box::new(MockRunner::new()),
            copy: |_| Ok(()),
            open: |_| Ok(()),
            confirm: |_, _| Ok(()),
            signal: |_, _| Ok(()),
        }
    }

    /// The id has to survive a port being reused by a different process: it
    /// carries the pid, the port, the start time, and the addresses, so a
    /// selection cannot silently follow a port to a new owner.
    #[test]
    fn a_listener_id_distinguishes_a_reused_port_from_the_original() {
        let original = entry(3000, 10, None, true);
        let mut reused = original.clone();
        reused.identity.start_time = 99;
        let mut other_pid = original.clone();
        other_pid.identity.pid = 11;
        let mut other_address = original.clone();
        other_address.identity.addresses = vec!["0.0.0.0".parse().unwrap()];

        assert_eq!(port_id(&original), "10:3000:42:127.0.0.1");
        for different in [reused, other_pid, other_address] {
            assert_ne!(port_id(&original), port_id(&different));
        }
    }

    /// The card is the only place a listener explains itself, and three of its
    /// rows are conditional.
    #[test]
    fn a_listener_card_states_every_fact_it_has_and_omits_the_ones_it_does_not() {
        let full = port_item(&entry(3000, 10, Some("/work/api"), true));
        assert_eq!(full.primary, ":3000");
        assert!(full.secondary.contains("node"), "{}", full.secondary);
        assert!(full.secondary.contains("pid 10"), "{}", full.secondary);
        let card = full.preview.join("\n");
        assert!(card.contains("endpoint  localhost:3000"), "{card}");
        assert!(card.contains("127.0.0.1, ::1"), "{card}");
        assert!(card.contains("ppid      9"), "{card}");
        assert!(card.contains("cwd       /work/api"), "{card}");
        assert!(!card.contains("signal    disabled"), "{card}");

        // No cwd, no owner: the two rows that depend on them drop out, and the
        // one that only appears when signalling is impossible appears.
        let bare = port_item(&entry(3000, 10, None, false)).preview.join("\n");
        assert!(!bare.contains("cwd  "), "{bare}");
        assert!(bare.contains("signal    disabled"), "{bare}");
    }

    /// A row without a user reads `unknown` rather than an empty column.
    #[test]
    fn a_listener_with_no_owner_still_names_a_user() {
        let mut anonymous = entry(3000, 10, None, true);
        anonymous.user = None;
        anonymous.parent_pid = None;
        let card = port_item(&anonymous).preview.join("\n");
        assert!(card.contains("user      unknown"), "{card}");
        assert!(!card.contains("ppid"), "{card}");
    }

    /// Every filter field the schema advertises has to actually resolve against
    /// the document the item builder produces, or a documented query silently
    /// matches nothing.
    #[test]
    fn every_advertised_filter_field_matches_the_item_it_describes() {
        let mode = mode(vec![entry(3000, 10, Some("/work/api"), true)]);
        let item = &mode.items()[0];
        let schema = mode.schema();
        let mut matcher = nucleo_matcher::Matcher::new(nucleo_matcher::Config::DEFAULT);

        for query in [
            "port:3000",
            "address:127.0.0.1",
            "pid:10",
            "process:node",
            "proc:server.js",
            "cwd:/work/api",
            "repo:api",
            "user:dev",
        ] {
            let compiled = crate::query::CompiledQuery::compile(query, &schema)
                .unwrap_or_else(|error| panic!("`{query}` did not compile: {error:?}"));
            assert!(
                compiled.score(&item.document, &mut matcher).is_some(),
                "`{query}` matched no listener"
            );
        }
    }

    /// Both destructive actions and the workspace action are refused when their
    /// precondition is missing, and the refusal says why — that reason is what
    /// the command bar shows instead of the pill.
    #[test]
    fn actions_are_disabled_with_a_reason_when_their_precondition_is_missing() {
        let owned = entry(3000, 10, Some("/definitely/not/here"), true);
        let foreign = entry(3001, 11, None, false);
        let id_owned = port_id(&owned);
        let id_foreign = port_id(&foreign);
        let mode = mode(vec![owned, foreign]);

        let missing_cwd = mode
            .action_disabled_reason(&id_owned, "workspace")
            .expect("a cwd that is not a directory disables workspace");
        assert!(missing_cwd.contains("cwd"), "{missing_cwd}");

        for action in ["term", "kill"] {
            let reason = mode
                .action_disabled_reason(&id_foreign, action)
                .unwrap_or_else(|| panic!("{action} must be disabled for a foreign listener"));
            assert!(reason.contains("not owned"), "{reason}");
        }
        // Signalling your own process is allowed, and so is copying anything.
        assert!(mode.action_disabled_reason(&id_owned, "term").is_none());
        assert!(mode.action_disabled_reason(&id_foreign, "copy").is_none());
        // An id nothing matches disables nothing rather than panicking.
        assert!(mode.action_disabled_reason("gone", "term").is_none());
    }

    /// The list starts empty and fills from the worker, so `poll` is the only
    /// path that installs entries.
    #[test]
    fn polling_installs_the_newest_snapshot_and_reports_a_failed_one() {
        let mut ok = mode(Vec::new());
        assert!(ok.initial().unwrap().is_empty());
        assert!(ok.is_polling(), "the refresh worker always runs");

        // Two snapshots queued: only the newest is taken.
        ok.worker = PortWorker::seeded(vec![
            Ok(vec![entry(3000, 10, None, true)]),
            Ok(vec![
                entry(3001, 11, None, true),
                entry(3002, 12, None, true),
            ]),
        ]);
        let items = ok.poll().expect("a snapshot was queued").unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].primary, ":3001");
        assert!(ok.poll().is_none(), "the queue is drained");

        let mut broken = mode(Vec::new());
        broken.worker = PortWorker::seeded(vec![Err("probe exploded".into())]);
        let error = broken.poll().expect("a result was queued").unwrap_err();
        assert!(error.to_string().contains("probe exploded"), "{error}");
    }

    /// Executing against an id that no longer exists must fail rather than act
    /// on whatever is nearest — the whole point of the composite id.
    #[test]
    fn executing_against_a_vanished_listener_fails_before_doing_anything() {
        let mut mode = mode(vec![entry(3000, 10, None, true)]);
        let error = mode.execute("11:3000:42:127.0.0.1", "copy").unwrap_err();
        assert!(error.to_string().contains("disappeared"), "{error}");

        let id = port_id(&mode.entries[0].clone());
        let error = mode.execute(&id, "not-an-action").unwrap_err();
        assert!(error.to_string().contains("unknown port action"), "{error}");
    }

    /// The chrome a mode declares is what the shared picker renders, so a
    /// missing or renamed action is a pill that silently stops existing.
    #[test]
    fn the_mode_declares_its_title_accent_and_every_action_it_answers_to() {
        let mode = mode(Vec::new());
        assert_eq!(mode.title(), "Ports");
        assert_eq!(mode.accent_slot(), "teal");
        let ids: Vec<&str> = mode.actions().iter().map(|action| action.id).collect();
        assert_eq!(ids, ["copy", "http", "https", "workspace", "term", "kill"]);
        // Every action carries a key cap, or the command bar renders a blank pill.
        assert!(mode.actions().iter().all(|a| !a.key_label.is_empty()));
        crate::picker::assert_follows_prefix_concept("ports", &mode.actions());
    }

    /// A signal is refused outright for a listener the user does not own, before
    /// anything is printed or read.
    #[test]
    fn confirming_a_signal_refuses_a_listener_the_user_does_not_own() {
        let error = confirm_signal(&entry(3000, 10, None, false), false).unwrap_err();
        assert!(error.to_string().contains("another user"), "{error}");
    }

    fn mode_with(entries: Vec<PortEntry>, runner: MockRunner) -> (PortMode, &'static MockRunner) {
        let runner = runner.leak();
        let mut mode = mode(entries);
        mode.runner = Box::new(runner);
        (mode, runner)
    }

    #[test]
    fn copy_and_open_hand_the_endpoint_to_their_effect() {
        let item = entry(3000, 10, None, true);
        let id = port_id(&item);
        let (mut mode, runner) = mode_with(vec![item], MockRunner::new());
        mode.copy = |text| {
            anyhow::ensure!(text == "localhost:3000", "copied {text}");
            Ok(())
        };
        mode.open = |url| {
            anyhow::ensure!(url.ends_with("://localhost:3000"), "opened {url}");
            Ok(())
        };
        for action in ["copy", "http", "https"] {
            assert!(matches!(
                mode.execute(&id, action).unwrap(),
                ActionOutcome::Close
            ));
        }
        assert!(runner.calls().is_empty());
        assert!(mode.execute(&id, "teleport").is_err());
        assert!(mode.execute("gone", "copy").is_err());
    }

    #[test]
    fn a_workspace_opens_in_the_listeners_cwd_named_after_it() {
        let dir = std::env::temp_dir();
        let label = dir.file_name().unwrap().to_string_lossy().into_owned();
        let item = entry(3000, 10, Some(dir.to_str().unwrap()), true);
        let id = port_id(&item);
        let (mut mode, runner) = mode_with(vec![item], MockRunner::new());
        mode.execute(&id, "workspace").unwrap();
        assert_eq!(
            runner.calls(),
            vec![vec![
                "herdr".to_string(),
                "workspace".into(),
                "create".into(),
                "--cwd".into(),
                dir.to_string_lossy().into_owned(),
                "--label".into(),
                label,
                "--focus".into(),
            ]]
        );

        let (mut failing, _) = mode_with(
            vec![entry(3000, 10, Some(dir.to_str().unwrap()), true)],
            MockRunner::new().failing("herdr"),
        );
        assert!(failing.execute(&id, "workspace").is_err());

        let gone = entry(4000, 11, Some("/definitely/gone"), true);
        let gone_id = port_id(&gone);
        let hidden = entry(5000, 12, None, true);
        let hidden_id = port_id(&hidden);
        let (mut mode, runner) = mode_with(vec![gone, hidden], MockRunner::new());
        assert!(mode.execute(&gone_id, "workspace").is_err());
        assert!(mode.execute(&hidden_id, "workspace").is_err());
        assert!(runner.calls().is_empty(), "nothing opens without a cwd");
    }

    #[test]
    fn term_and_kill_confirm_then_signal_and_report_failures() {
        let item = entry(3000, 10, None, true);
        let id = port_id(&item);
        let (mut mode, _) = mode_with(vec![item], MockRunner::new());
        mode.signal = |identity, _| {
            anyhow::ensure!(identity.port == 3000, "wrong listener");
            Ok(())
        };
        mode.execute(&id, "term").unwrap();
        mode.execute(&id, "kill").unwrap();

        mode.signal = |_, _| anyhow::bail!("listener is stale or no longer signalable");
        assert!(mode.execute(&id, "term").is_err());
        mode.signal = |_, _| anyhow::bail!("could not send signal");
        assert!(mode.execute(&id, "kill").is_err());

        // A refused confirmation never reaches the signal.
        mode.confirm = |_, _| anyhow::bail!("signal cancelled");
        mode.signal = |_, _| panic!("signalled without confirmation");
        assert!(mode.execute(&id, "term").is_err());
    }

    #[test]
    fn a_signal_is_confirmed_only_by_typing_its_own_word() {
        let item = entry(3000, 10, None, true);
        let mut shown = Vec::new();
        confirm_signal_with(&item, false, &mut "term\n".as_bytes(), &mut shown).unwrap();
        let shown = String::from_utf8(shown).unwrap();
        assert!(shown.contains("Stop process on port 3000"), "{shown}");
        assert!(shown.contains("node server.js"), "{shown}");

        let mut out = Vec::new();
        confirm_signal_with(&item, true, &mut "kill\n".as_bytes(), &mut out).unwrap();
        assert!(String::from_utf8(out).unwrap().contains("Force kill"));
        assert!(
            confirm_signal_with(&item, true, &mut "term\n".as_bytes(), &mut Vec::new()).is_err()
        );
    }

    /// The real probe, against a listener this test owns: it must be found
    /// under this process, attributed to this user, and described.
    #[test]
    fn the_system_probe_sees_a_listener_this_process_opened() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let pid = std::process::id();

        let mut probe = SystemProbe::new();
        let found = probe.listeners().expect("listeners readable");
        assert!(
            found.iter().any(|l| l.port == port && l.pid == pid),
            "own listener on {port} not reported"
        );
        let meta = probe.process(pid).expect("own process is visible");
        assert!(meta.owned_by_current_user);
        assert!(meta.start_time > 0);
        assert!(probe.process(u32::MAX).is_none());
        assert!(probe.signal(u32::MAX, PortSignal::Term).is_err());
    }

    /// Signals reach a real child of this test, and only after revalidation.
    #[test]
    fn the_system_probe_signals_a_process_this_test_started() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("sleep");
        let mut probe = SystemProbe::new();
        probe
            .signal(child.id(), PortSignal::Term)
            .expect("own child accepts TERM");
        let status = child.wait().expect("child exits");
        assert!(!status.success(), "terminated by the signal");

        // The revalidating path refuses an identity that is not listening.
        let stale = PortIdentity {
            pid: std::process::id(),
            port: 1,
            start_time: 0,
            addresses: Vec::new(),
        };
        assert!(system_signal(&stale, PortSignal::Kill).is_err());
    }

    /// The live worker scans, delivers, and stops when dropped.
    #[test]
    fn the_live_worker_delivers_a_snapshot_and_stops_on_drop() {
        let worker = PortWorker::start(Duration::from_millis(50));
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let snapshot = loop {
            if let Some(snapshot) = worker.latest() {
                break snapshot;
            }
            assert!(std::time::Instant::now() < deadline, "no snapshot");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert!(snapshot.is_ok(), "{snapshot:?}");
        drop(worker);
    }

    /// Both failure shapes for both signals: a stale listener and a refused
    /// signal each surface as an error.
    #[test]
    fn either_signal_reports_a_stale_listener_or_a_refusal() {
        let item = entry(3000, 10, None, true);
        let id = port_id(&item);
        let (mut mode, _) = mode_with(vec![item], MockRunner::new());
        for (stale, action) in [(true, "kill"), (false, "term")] {
            mode.signal = if stale {
                |_, _| anyhow::bail!("listener is stale or no longer signalable")
            } else {
                |_, _| anyhow::bail!("could not send signal")
            };
            assert!(mode.execute(&id, action).is_err());
        }
    }

    /// The live mode starts its own scanner, reloads it with new settings, and
    /// folds a snapshot into rows.
    #[test]
    fn the_live_mode_scans_reloads_and_lists() {
        let mut mode = PortMode::new(50, Notifier::silent(), HashMap::new());
        assert!(mode.initial().unwrap().is_empty());
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(snapshot) = mode.poll() {
                assert!(snapshot.is_ok());
                break;
            }
            assert!(std::time::Instant::now() < deadline, "no snapshot");
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut cfg = Config::default();
        cfg.ports.refresh_interval_ms = 250;
        cfg.keys.insert(
            "ports".into(),
            HashMap::from([("copy".into(), "ctrl-k".into())]),
        );
        mode.reload_config(&cfg).unwrap();
        assert!(mode.entries.is_empty());
        assert_eq!(
            mode.bindings.get("copy").map(String::as_str),
            Some("ctrl-k")
        );
        let _ = mode.key_bindings();
    }
}
