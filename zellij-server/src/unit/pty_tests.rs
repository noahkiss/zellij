use super::*;
use crate::os_input_output::ServerOsApi;
use crate::plugins::PluginInstruction;
use crate::session_layout_metadata::PaneLayoutMetadata;
use crate::thread_bus::Bus;
use interprocess::local_socket::Stream as LocalSocketStream;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use zellij_utils::channels::{self, SenderWithContext};
use zellij_utils::data::{Event, Palette};
use zellij_utils::errors::ErrorContext;
use zellij_utils::input::command::RunCommand;
use zellij_utils::ipc::{ClientToServerMsg, IpcReceiverWithContext, ServerToClientMsg};

type QuitCb = Box<dyn Fn(PaneId, Option<i32>, RunCommand) + Send>;

#[derive(Clone)]
struct MockOsApi {
    cwds: Arc<Mutex<HashMap<u32, PathBuf>>>,
    cmds: Arc<Mutex<HashMap<u32, Vec<String>>>>,
    cmds_by_ppid: Arc<Mutex<HashMap<String, Vec<String>>>>,
    /// the callback the pty thread hands to a spawn, kept so a test can make the process exit
    quit_cb: Arc<Mutex<Option<QuitCb>>>,
    /// Every signal this api was asked to deliver, as (pid, signal name).
    signals: Arc<Mutex<Vec<(u32, &'static str)>>>,
    /// terminal_id -> what that pane's pty fd answers as its foreground command, whatever pid the
    /// pane is asked about. Stands in for `tcgetpgrp` on a fd number another pane's pty reused.
    /// `clear_terminal_id` forgets the fd, as the real one does.
    foreground_by_terminal: Arc<Mutex<HashMap<u32, Vec<String>>>>,
}

impl MockOsApi {
    fn new() -> Self {
        MockOsApi {
            cwds: Arc::new(Mutex::new(HashMap::new())),
            cmds: Arc::new(Mutex::new(HashMap::new())),
            cmds_by_ppid: Arc::new(Mutex::new(HashMap::new())),
            quit_cb: Arc::new(Mutex::new(None)),
            signals: Arc::new(Mutex::new(Vec::new())),
            foreground_by_terminal: Arc::new(Mutex::new(HashMap::new())),
        }
    }
    fn set_terminal_foreground(&self, terminal_id: u32, cmd: Vec<String>) {
        self.foreground_by_terminal
            .lock()
            .unwrap()
            .insert(terminal_id, cmd);
    }
    fn knows_terminal_fd(&self, terminal_id: u32) -> bool {
        self.foreground_by_terminal
            .lock()
            .unwrap()
            .contains_key(&terminal_id)
    }
    /// Make the process behind the last spawned pane exit with this status.
    fn exit_last_spawned_pane(&self, pane_id: PaneId, exit_status: Option<i32>) {
        let quit_cb = self.quit_cb.lock().unwrap();
        let quit_cb = quit_cb
            .as_ref()
            .expect("the pty thread should have handed us a quit callback");
        quit_cb(pane_id, exit_status, RunCommand::default());
    }
    fn signals_sent(&self) -> Vec<(u32, &'static str)> {
        self.signals.lock().unwrap().clone()
    }
    fn set_cwd(&self, pid: u32, path: PathBuf) {
        self.cwds.lock().unwrap().insert(pid, path);
    }
    fn set_cmd(&self, pid: u32, cmd: Vec<String>) {
        self.cmds.lock().unwrap().insert(pid, cmd);
    }
    fn set_foreground_cmd(&self, ppid: u32, cmd: Vec<String>) {
        self.cmds_by_ppid
            .lock()
            .unwrap()
            .insert(ppid.to_string(), cmd);
    }
    fn clear_foreground_cmd(&self, ppid: u32) {
        self.cmds_by_ppid.lock().unwrap().remove(&ppid.to_string());
    }
}

impl ServerOsApi for MockOsApi {
    fn set_terminal_size_using_terminal_id(
        &self,
        _: u32,
        _: u16,
        _: u16,
        _: Option<u16>,
        _: Option<u16>,
    ) -> anyhow::Result<()> {
        Ok(())
    }
    fn spawn_terminal(
        &self,
        _: TerminalAction,
        quit_cb: Box<dyn Fn(PaneId, Option<i32>, RunCommand) + Send>,
        _: Option<PathBuf>,
    ) -> anyhow::Result<(u32, Box<dyn AsyncReader>, Option<u32>)> {
        *self.quit_cb.lock().unwrap() = Some(quit_cb);
        Ok((
            1,
            Box::new(crate::os_input_output::NullAsyncReader) as Box<dyn AsyncReader>,
            Some(100),
        ))
    }
    fn write_to_tty_stdin(&self, _: u32, buf: &[u8]) -> anyhow::Result<usize> {
        Ok(buf.len())
    }
    fn tcdrain(&self, _: u32) -> anyhow::Result<()> {
        Ok(())
    }
    fn kill(&self, pid: u32) -> anyhow::Result<()> {
        self.signals.lock().unwrap().push((pid, "HUP"));
        Ok(())
    }
    fn force_kill(&self, pid: u32) -> anyhow::Result<()> {
        self.signals.lock().unwrap().push((pid, "KILL"));
        Ok(())
    }
    fn send_sigint(&self, pid: u32) -> anyhow::Result<()> {
        self.signals.lock().unwrap().push((pid, "INT"));
        Ok(())
    }
    fn box_clone(&self) -> Box<dyn ServerOsApi> {
        Box::new((*self).clone())
    }
    fn send_to_client(&self, _: ClientId, _: ServerToClientMsg) -> anyhow::Result<()> {
        Ok(())
    }
    fn new_client(
        &mut self,
        _: ClientId,
        _: LocalSocketStream,
    ) -> anyhow::Result<IpcReceiverWithContext<ClientToServerMsg>> {
        unimplemented!()
    }
    fn new_client_with_reply(
        &mut self,
        _: ClientId,
        _: LocalSocketStream,
        _: LocalSocketStream,
    ) -> anyhow::Result<IpcReceiverWithContext<ClientToServerMsg>> {
        unimplemented!()
    }
    fn remove_client(&mut self, _: ClientId) -> anyhow::Result<()> {
        Ok(())
    }
    fn load_palette(&self) -> Palette {
        Palette::default()
    }
    fn get_cwd(&self, pid: u32) -> Option<PathBuf> {
        self.cwds.lock().unwrap().get(&pid).cloned()
    }
    fn get_cwds(&self, pids: Vec<u32>) -> (HashMap<u32, PathBuf>, HashMap<u32, Vec<String>>) {
        let cwds_lock = self.cwds.lock().unwrap();
        let cmds_lock = self.cmds.lock().unwrap();
        let cwds = pids
            .iter()
            .filter_map(|pid| cwds_lock.get(pid).map(|cwd| (*pid, cwd.clone())))
            .collect();
        let cmds = pids
            .iter()
            .filter_map(|pid| cmds_lock.get(pid).map(|cmd| (*pid, cmd.clone())))
            .collect();
        (cwds, cmds)
    }
    fn get_all_cmds_by_ppid(&self, _: &Option<String>) -> HashMap<String, Vec<String>> {
        self.cmds_by_ppid.lock().unwrap().clone()
    }
    fn get_foreground_cmds(
        &self,
        panes: &[(u32, u32)],
        _: &Option<String>,
    ) -> HashMap<u32, Vec<String>> {
        let cmds_by_ppid = self.cmds_by_ppid.lock().unwrap();
        panes
            .iter()
            .filter_map(|(terminal_id, shell_pid)| {
                cmds_by_ppid
                    .get(&shell_pid.to_string())
                    .map(|cmd| (*terminal_id, cmd.clone()))
            })
            .collect()
    }
    fn get_foreground_cmds_of_commands(
        &self,
        panes: &[(u32, u32)],
        _: &Option<String>,
    ) -> HashMap<u32, Vec<String>> {
        let cmds = self.cmds.lock().unwrap();
        let by_terminal = self.foreground_by_terminal.lock().unwrap();
        panes
            .iter()
            .filter_map(|(terminal_id, pid)| {
                by_terminal
                    .get(terminal_id)
                    .or_else(|| cmds.get(pid))
                    .map(|cmd| (*terminal_id, cmd.clone()))
            })
            .collect()
    }
    fn write_to_file(&mut self, _: String, _: Option<String>) -> anyhow::Result<()> {
        Ok(())
    }
    fn re_run_command_in_terminal(
        &self,
        _: u32,
        _: RunCommand,
        _: Box<dyn Fn(PaneId, Option<i32>, RunCommand) + Send>,
    ) -> anyhow::Result<(Box<dyn AsyncReader>, Option<u32>)> {
        unimplemented!()
    }
    fn clear_terminal_id(&self, terminal_id: u32) -> anyhow::Result<()> {
        self.foreground_by_terminal
            .lock()
            .unwrap()
            .remove(&terminal_id);
        Ok(())
    }
}

fn make_pty_with_plugin_receiver(
    mock: MockOsApi,
) -> (Pty, channels::Receiver<(PluginInstruction, ErrorContext)>) {
    let (plugin_tx, plugin_rx) = channels::unbounded();
    let plugin_sender = SenderWithContext::new(plugin_tx);
    let mut bus: Bus<PtyInstruction> = Bus::empty().should_silently_fail();
    bus.os_input = Some(Box::new(mock));
    bus.senders.to_plugin = Some(plugin_sender);
    let pty = Pty::new(bus, false, None, None, None, None, None, None);
    (pty, plugin_rx)
}

/// The other ends of the channels a real pty bus sends on, kept open so its sends land.
struct PtyBusReceivers {
    pty: channels::Receiver<(PtyInstruction, ErrorContext)>,
    _screen: channels::Receiver<(ScreenInstruction, ErrorContext)>,
    _plugin: channels::Receiver<(PluginInstruction, ErrorContext)>,
    _server: channels::Receiver<(crate::ServerInstruction, ErrorContext)>,
    _pty_writer: channels::Receiver<(crate::pty_writer::PtyWriteInstruction, ErrorContext)>,
    _background_jobs: channels::Receiver<(crate::background_jobs::BackgroundJob, ErrorContext)>,
}

/// The bus a real server gives the pty thread, built by the function the server builds it with.
fn real_pty_bus(mock: MockOsApi) -> (Bus<PtyInstruction>, PtyBusReceivers) {
    let (_, pty_receiver_of_bus) = channels::unbounded::<(PtyInstruction, ErrorContext)>();
    let (pty_tx, pty_rx) = channels::unbounded();
    let (screen_tx, screen_rx) = channels::unbounded();
    let (plugin_tx, plugin_rx) = channels::unbounded();
    let (server_tx, server_rx) = channels::unbounded();
    let (pty_writer_tx, pty_writer_rx) = channels::unbounded();
    let (background_jobs_tx, background_jobs_rx) = channels::unbounded();
    let bus = crate::pty_thread_bus(
        pty_receiver_of_bus,
        &SenderWithContext::new(screen_tx),
        &SenderWithContext::new(pty_tx),
        &SenderWithContext::new(plugin_tx),
        &SenderWithContext::new(server_tx),
        &SenderWithContext::new(pty_writer_tx),
        &SenderWithContext::new(background_jobs_tx),
        Box::new(mock),
    );
    let receivers = PtyBusReceivers {
        pty: pty_rx,
        _screen: screen_rx,
        _plugin: plugin_rx,
        _server: server_rx,
        _pty_writer: pty_writer_rx,
        _background_jobs: background_jobs_rx,
    };
    (bus, receivers)
}

/// A pty whose own instruction channel a test can read, to see what a quit callback sent back.
///
/// Its bus is the one a real server builds, so a test that reads the pty channel tests the wiring
/// too, not a channel the test added.
fn make_pty_with_pty_receiver(
    mock: MockOsApi,
) -> (Pty, channels::Receiver<(PtyInstruction, ErrorContext)>) {
    let (mut bus, receivers) = real_pty_bus(mock);
    // the other receivers go with `receivers`; their sends are not what these tests read
    bus.senders.should_silently_fail = true;
    let pty = Pty::new(bus, false, None, None, None, None, None, None);
    (pty, receivers.pty)
}

/// A pty whose screen channel a test can read, to see what each tick told Screen.
fn make_pty_with_screen_receiver(
    mock: MockOsApi,
) -> (Pty, channels::Receiver<(ScreenInstruction, ErrorContext)>) {
    let (plugin_tx, _plugin_rx) = channels::unbounded();
    let (screen_tx, screen_rx) = channels::unbounded();
    let mut bus: Bus<PtyInstruction> = Bus::empty().should_silently_fail();
    bus.os_input = Some(Box::new(mock));
    bus.senders.to_plugin = Some(SenderWithContext::new(plugin_tx));
    bus.senders.to_screen = Some(SenderWithContext::new(screen_tx));
    let pty = Pty::new(bus, false, None, None, None, None, None, None);
    (pty, screen_rx)
}

fn collect_process_info_reports(
    rx: &channels::Receiver<(ScreenInstruction, ErrorContext)>,
) -> Vec<HashMap<u32, PaneProcessInfo>> {
    let mut reports = Vec::new();
    while let Ok((instruction, _)) = rx.try_recv() {
        if let ScreenInstruction::UpdatePaneProcessInfo { process_info, .. } = instruction {
            reports.push(process_info);
        }
    }
    reports
}

fn set_active_terminal(pty: &mut Pty, terminal_id: u32, child_pid: u32) {
    let flag = Arc::new(AtomicBool::new(true));
    pty.id_to_child_pid.insert(terminal_id, child_pid);
    pty.pane_activity_flags.insert(terminal_id, flag);
}

fn collect_cwd_changed_events(
    rx: &channels::Receiver<(PluginInstruction, ErrorContext)>,
) -> Vec<(PaneId, PathBuf)> {
    let mut events = Vec::new();
    while let Ok((instruction, _)) = rx.try_recv() {
        if let PluginInstruction::Update(updates) = instruction {
            for (_, _, event) in updates {
                if let Event::CwdChanged(pane_id, cwd, _) = event {
                    events.push((pane_id.into(), cwd));
                }
            }
        }
    }
    events
}

fn collect_command_changed_events(
    rx: &channels::Receiver<(PluginInstruction, ErrorContext)>,
) -> Vec<(PaneId, Vec<String>, bool)> {
    let mut events = Vec::new();
    while let Ok((instruction, _)) = rx.try_recv() {
        if let PluginInstruction::Update(updates) = instruction {
            for (_, _, event) in updates {
                if let Event::CommandChanged(pane_id, cmd, is_foreground, _) = event {
                    events.push((pane_id.into(), cmd, is_foreground));
                }
            }
        }
    }
    events
}

#[test]
fn foreground_command_emitted_with_is_foreground_true() {
    let mock = MockOsApi::new();
    let child_pid = 100;
    mock.set_foreground_cmd(child_pid, vec!["vim".into(), "file.rs".into()]);
    let (mut pty, rx) = make_pty_with_plugin_receiver(mock);
    set_active_terminal(&mut pty, 1, child_pid);

    pty.update_and_report_cwds();

    let events = collect_command_changed_events(&rx);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0, PaneId::Terminal(1));
    assert_eq!(events[0].1, vec!["vim", "file.rs"]);
    assert!(events[0].2, "expected is_foreground=true");
}

#[test]
fn empty_foreground_falls_back_to_shell_command() {
    let mock = MockOsApi::new();
    let child_pid = 100;
    mock.set_cmd(child_pid, vec!["/bin/bash".into()]);
    let (mut pty, rx) = make_pty_with_plugin_receiver(mock);
    set_active_terminal(&mut pty, 1, child_pid);

    pty.update_and_report_cwds();

    let events = collect_command_changed_events(&rx);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0, PaneId::Terminal(1));
    assert_eq!(events[0].1, vec!["/bin/bash"]);
    assert!(!events[0].2, "expected is_foreground=false");
}

#[test]
fn foreground_clearing_emits_shell_fallback() {
    let mock = MockOsApi::new();
    let child_pid = 100;
    mock.set_cmd(child_pid, vec!["/bin/zsh".into()]);
    mock.set_foreground_cmd(child_pid, vec!["cargo".into(), "build".into()]);
    let (mut pty, rx) = make_pty_with_plugin_receiver(mock.clone());
    set_active_terminal(&mut pty, 1, child_pid);

    pty.update_and_report_cwds();
    let events = collect_command_changed_events(&rx);
    assert_eq!(events.len(), 1);
    assert!(events[0].2, "first event should be foreground");
    assert_eq!(events[0].1, vec!["cargo", "build"]);

    mock.clear_foreground_cmd(child_pid);
    pty.pane_activity_flags
        .get(&1)
        .unwrap()
        .store(true, Ordering::Relaxed);

    pty.update_and_report_cwds();
    let events = collect_command_changed_events(&rx);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].1, vec!["/bin/zsh"]);
    assert!(
        !events[0].2,
        "after clearing foreground, should fall back to shell"
    );
}

#[test]
fn no_event_when_foreground_unchanged() {
    let mock = MockOsApi::new();
    let child_pid = 100;
    mock.set_foreground_cmd(child_pid, vec!["htop".into()]);
    let (mut pty, rx) = make_pty_with_plugin_receiver(mock);
    set_active_terminal(&mut pty, 1, child_pid);

    pty.update_and_report_cwds();
    let _ = collect_command_changed_events(&rx);

    pty.pane_activity_flags
        .get(&1)
        .unwrap()
        .store(true, Ordering::Relaxed);
    pty.update_and_report_cwds();
    let events = collect_command_changed_events(&rx);
    assert!(
        events.is_empty(),
        "no event expected when command unchanged"
    );
}

#[test]
fn no_event_for_inactive_terminal() {
    let mock = MockOsApi::new();
    let child_pid = 100;
    mock.set_foreground_cmd(child_pid, vec!["vim".into()]);
    let (mut pty, rx) = make_pty_with_plugin_receiver(mock);
    set_active_terminal(&mut pty, 1, child_pid);
    pty.pane_activity_flags
        .get(&1)
        .unwrap()
        .store(false, Ordering::Relaxed);

    pty.update_and_report_cwds();
    let events = collect_command_changed_events(&rx);
    assert!(
        events.is_empty(),
        "inactive terminal should produce no events"
    );
}

#[test]
fn foreground_change_between_two_commands() {
    let mock = MockOsApi::new();
    let child_pid = 100;
    mock.set_foreground_cmd(child_pid, vec!["vim".into()]);
    let (mut pty, rx) = make_pty_with_plugin_receiver(mock.clone());
    set_active_terminal(&mut pty, 1, child_pid);

    pty.update_and_report_cwds();
    let events = collect_command_changed_events(&rx);
    assert_eq!(events[0].1, vec!["vim"]);
    assert!(events[0].2);

    mock.set_foreground_cmd(child_pid, vec!["cargo".into(), "test".into()]);
    pty.pane_activity_flags
        .get(&1)
        .unwrap()
        .store(true, Ordering::Relaxed);

    pty.update_and_report_cwds();
    let events = collect_command_changed_events(&rx);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].1, vec!["cargo", "test"]);
    assert!(events[0].2);
}

// --- Activity flag gating ---

#[test]
fn activity_flag_reset_after_poll() {
    let mock = MockOsApi::new();
    let child_pid = 100;
    let (mut pty, _rx) = make_pty_with_plugin_receiver(mock);
    set_active_terminal(&mut pty, 1, child_pid);
    assert!(pty
        .pane_activity_flags
        .get(&1)
        .unwrap()
        .load(Ordering::Relaxed));

    pty.update_and_report_cwds();

    assert!(
        !pty.pane_activity_flags
            .get(&1)
            .unwrap()
            .load(Ordering::Relaxed),
        "activity flag should be reset to false after poll"
    );
}

#[test]
fn multiple_terminals_only_active_ones_polled() {
    let mock = MockOsApi::new();
    let pid_active = 100;
    let pid_inactive = 200;
    mock.set_cwd(pid_active, PathBuf::from("/active"));
    mock.set_cwd(pid_inactive, PathBuf::from("/inactive"));
    let (mut pty, rx) = make_pty_with_plugin_receiver(mock);
    set_active_terminal(&mut pty, 1, pid_active);
    set_active_terminal(&mut pty, 2, pid_inactive);
    pty.pane_activity_flags
        .get(&2)
        .unwrap()
        .store(false, Ordering::Relaxed);

    pty.update_and_report_cwds();

    let events = collect_cwd_changed_events(&rx);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0, PaneId::Terminal(1));
    assert_eq!(events[0].1, PathBuf::from("/active"));
}

// --- CWD change events ---

#[test]
fn cwd_changed_event_emitted_on_change() {
    let mock = MockOsApi::new();
    let child_pid = 100;
    mock.set_cwd(child_pid, PathBuf::from("/home/user"));
    let (mut pty, rx) = make_pty_with_plugin_receiver(mock);
    set_active_terminal(&mut pty, 1, child_pid);

    pty.update_and_report_cwds();

    let events = collect_cwd_changed_events(&rx);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0, PaneId::Terminal(1));
    assert_eq!(events[0].1, PathBuf::from("/home/user"));
}

#[test]
fn no_cwd_event_when_unchanged() {
    let mock = MockOsApi::new();
    let child_pid = 100;
    mock.set_cwd(child_pid, PathBuf::from("/home/user"));
    let (mut pty, rx) = make_pty_with_plugin_receiver(mock);
    set_active_terminal(&mut pty, 1, child_pid);
    pty.terminal_cwds.insert(1, PathBuf::from("/home/user"));

    pty.update_and_report_cwds();

    let events = collect_cwd_changed_events(&rx);
    assert!(events.is_empty(), "no event expected when cwd unchanged");
}

// --- OSC7 CWD notification ---

#[test]
fn osc7_emits_cwd_changed() {
    let mock = MockOsApi::new();
    let (mut pty, rx) = make_pty_with_plugin_receiver(mock);
    pty.id_to_child_pid.insert(1, 100);

    pty.notify_cwd_from_osc7(1, PathBuf::from("/tmp/new"));

    let events = collect_cwd_changed_events(&rx);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].0, PaneId::Terminal(1));
    assert_eq!(events[0].1, PathBuf::from("/tmp/new"));
    assert_eq!(
        pty.terminal_cwds.get(&1),
        Some(&PathBuf::from("/tmp/new")),
        "cache should be updated"
    );
}

#[test]
fn osc7_no_event_when_unchanged() {
    let mock = MockOsApi::new();
    let (mut pty, rx) = make_pty_with_plugin_receiver(mock);
    pty.id_to_child_pid.insert(1, 100);
    pty.terminal_cwds.insert(1, PathBuf::from("/same"));

    pty.notify_cwd_from_osc7(1, PathBuf::from("/same"));

    let events = collect_cwd_changed_events(&rx);
    assert!(events.is_empty(), "no event when osc7 path matches cache");
}

/// The flag says "this pane produced output", and OSC 7 arrives as output. Clearing it here used
/// to save one cwd read and cost the pane its whole tick - command discovery included.
#[test]
fn osc7_leaves_the_activity_flag_alone() {
    let mock = MockOsApi::new();
    let (mut pty, _rx) = make_pty_with_plugin_receiver(mock);
    let flag = Arc::new(AtomicBool::new(true));
    pty.id_to_child_pid.insert(1, 100);
    pty.pane_activity_flags.insert(1, flag.clone());

    pty.notify_cwd_from_osc7(1, PathBuf::from("/new"));

    assert!(
        flag.load(Ordering::Relaxed),
        "osc7 should leave the activity flag set - the pane did produce output"
    );
}

/// A shell that reports its cwd at every prompt must not starve its own command discovery.
#[test]
fn osc7_then_poll_still_discovers_the_command() {
    let mock = MockOsApi::new();
    let child_pid = 100;
    mock.set_cwd(child_pid, PathBuf::from("/from-osc7"));
    mock.set_foreground_cmd(child_pid, vec!["vim".into()]);
    let (mut pty, rx) = make_pty_with_plugin_receiver(mock);
    set_active_terminal(&mut pty, 1, child_pid);

    pty.notify_cwd_from_osc7(1, PathBuf::from("/from-osc7"));
    let osc7_events = collect_cwd_changed_events(&rx);
    assert_eq!(osc7_events.len(), 1);

    pty.update_and_report_cwds();
    // one drain of the channel, so collect the events this test is about
    let cmd_events = collect_command_changed_events(&rx);
    assert_eq!(
        cmd_events.len(),
        1,
        "the poll must still discover what the pane is running"
    );
}

/// A launcher hands the server no SHELL, and the server hands its environment to every pane: the
/// bare `sh` that used to follow is a session that looks up while none of the user's shell is
/// there.
#[test]
#[cfg(not(windows))]
fn a_pane_with_no_shell_in_the_environment_gets_the_one_from_the_passwd_entry() {
    assert_eq!(
        default_shell_from(None, || Some(PathBuf::from("/usr/bin/fish"))),
        PathBuf::from("/usr/bin/fish")
    );
    // an empty SHELL is a variable nothing filled in, not a choice
    assert_eq!(
        default_shell_from(Some(""), || Some(PathBuf::from("/usr/bin/fish"))),
        PathBuf::from("/usr/bin/fish")
    );
}

#[test]
#[cfg(not(windows))]
fn what_the_environment_says_wins_and_bin_sh_is_still_the_last_resort() {
    assert_eq!(
        default_shell_from(Some("/bin/zsh"), || Some(PathBuf::from("/usr/bin/fish"))),
        PathBuf::from("/bin/zsh")
    );
    assert_eq!(default_shell_from(None, || None), PathBuf::from("/bin/sh"));
}

fn collect_pane_exited_events(
    rx: &channels::Receiver<(PluginInstruction, ErrorContext)>,
) -> Vec<(PaneId, Option<i32>)> {
    let mut events = Vec::new();
    while let Ok((instruction, _)) = rx.try_recv() {
        if let PluginInstruction::Update(updates) = instruction {
            for (plugin_id, client_id, event) in updates {
                if let Event::PaneExited(pane_id, exit_status) = event {
                    assert!(
                        plugin_id.is_none() && client_id.is_none(),
                        "PaneExited must be broadcast, not targeted at {:?}/{:?}",
                        plugin_id,
                        client_id
                    );
                    events.push((pane_id.into(), exit_status));
                }
            }
        }
    }
    events
}

/// A command pane nobody is subscribed to could fail and tell nobody: `CommandPaneExited` goes
/// only to the plugin that opened the pane, and a layout or the CLI is not a plugin.
#[test]
fn a_failed_command_pane_broadcasts_its_exit_status() {
    let mock = MockOsApi::new();
    let (mut pty, rx) = make_pty_with_plugin_receiver(mock.clone());
    let run_command = RunCommand {
        command: PathBuf::from("false"),
        hold_on_close: true,
        ..Default::default()
    };
    let _ = pty
        .spawn_terminal(
            Some(TerminalAction::RunCommand(run_command)),
            ClientTabIndexOrPaneId::TabIndex(0),
        )
        .expect("the mock spawns a terminal");
    // drain everything the spawn itself reported
    let _ = collect_pane_exited_events(&rx);

    mock.exit_last_spawned_pane(PaneId::Terminal(1), Some(1));

    let events = collect_pane_exited_events(&rx);
    assert_eq!(
        events,
        vec![(PaneId::Terminal(1), Some(1))],
        "a command pane that failed should broadcast its exit status"
    );
}

/// A process killed by a signal has no exit status, and saying so is not the same as saying zero.
#[test]
fn a_pane_killed_by_a_signal_broadcasts_no_exit_status() {
    let mock = MockOsApi::new();
    let (mut pty, rx) = make_pty_with_plugin_receiver(mock.clone());
    let _ = pty
        .spawn_terminal(None, ClientTabIndexOrPaneId::TabIndex(0))
        .expect("the mock spawns a terminal");
    let _ = collect_pane_exited_events(&rx);

    mock.exit_last_spawned_pane(PaneId::Terminal(1), None);

    let events = collect_pane_exited_events(&rx);
    assert_eq!(events, vec![(PaneId::Terminal(1), None)]);
}

#[test]
fn each_signal_reaches_the_pane_process() {
    for (signal, expected) in [
        (PaneSignal::Int, "INT"),
        (PaneSignal::Hup, "HUP"),
        (PaneSignal::Kill, "KILL"),
    ] {
        let mock = MockOsApi::new();
        let recorder = mock.clone();
        let (mut pty, _rx) = make_pty_with_plugin_receiver(mock);
        set_active_terminal(&mut pty, 1, 4242);

        pty.signal_pane(PaneId::Terminal(1), signal).expect("TEST");
        assert_eq!(recorder.signals_sent(), vec![(4242, expected)]);
    }
}

/// The pid of a reaped child is the OS's to hand out again, so a pane that outlives its command
/// must not keep it: signalling it would hit whatever process holds that number now.
#[test]
fn a_held_pane_forgets_the_pid_of_its_reaped_child() {
    let mock = MockOsApi::new();
    let recorder = mock.clone();
    let (mut pty, pty_rx) = make_pty_with_pty_receiver(mock.clone());
    let held_command = RunCommand {
        command: PathBuf::from("/bin/does-not-matter"),
        hold_on_close: true,
        ..Default::default()
    };
    let _ = pty
        .spawn_terminal(
            Some(TerminalAction::RunCommand(held_command)),
            ClientTabIndexOrPaneId::TabIndex(0),
        )
        .expect("the mock spawns a terminal");
    assert_eq!(
        pty.id_to_child_pid.get(&1).copied(),
        Some(100),
        "the running pane has a pid"
    );

    mock.exit_last_spawned_pane(PaneId::Terminal(1), Some(0));

    // the reaping thread owns none of the pty thread's state, so it asks the pty thread to forget
    let mut forgotten = vec![];
    while let Ok((instruction, _)) = pty_rx.try_recv() {
        if let PtyInstruction::ChildProcessExited(terminal_id) = instruction {
            forgotten.push(terminal_id);
        }
    }
    assert_eq!(
        forgotten,
        vec![1],
        "a held pane's exit should tell the pty thread to forget the child pid"
    );
    for terminal_id in forgotten {
        pty.forget_child_pid(terminal_id);
    }

    assert!(
        pty.id_to_child_pid.get(&1).is_none(),
        "a held pane holds no pid"
    );
    match pty.get_pane_pid(PaneId::Terminal(1)) {
        GetPanePidResponse::Err(_) => {},
        other => panic!("a held pane should report no pid, got {:?}", other),
    }
    let result = pty.signal_pane(PaneId::Terminal(1), PaneSignal::Kill);
    assert!(
        result.unwrap_err().contains("no running process"),
        "signalling a held pane should say there is no process, not signal a recycled pid"
    );
    assert!(
        recorder.signals_sent().is_empty(),
        "nothing should have been signalled"
    );
}

#[test]
fn signalling_a_pane_that_does_not_exist_is_an_error() {
    let mock = MockOsApi::new();
    let recorder = mock.clone();
    let (mut pty, _rx) = make_pty_with_plugin_receiver(mock);
    set_active_terminal(&mut pty, 1, 4242);

    let result = pty.signal_pane(PaneId::Terminal(7), PaneSignal::Int);
    assert!(result.is_err(), "no pane 7");
    assert!(
        result.unwrap_err().contains("no running process"),
        "the error names the miss"
    );
    assert!(
        recorder.signals_sent().is_empty(),
        "and nothing was signalled"
    );
}

#[test]
fn a_plugin_pane_has_no_process_to_signal() {
    let mock = MockOsApi::new();
    let recorder = mock.clone();
    let (mut pty, _rx) = make_pty_with_plugin_receiver(mock);
    set_active_terminal(&mut pty, 1, 4242);

    let result = pty.signal_pane(PaneId::Plugin(1), PaneSignal::Kill);
    assert!(result.is_err(), "a plugin pane runs no process");
    assert!(recorder.signals_sent().is_empty());
}

/// An idle session should not wake the screen thread once a second to tell it nothing.
#[test]
fn an_unchanged_process_info_map_is_not_reported_again() {
    let mock = MockOsApi::new();
    let child_pid = 100;
    mock.set_cmd(child_pid, vec!["/bin/bash".into()]);
    mock.set_cwd(child_pid, PathBuf::from("/tmp"));
    let (mut pty, screen_rx) = make_pty_with_screen_receiver(mock.clone());
    set_active_terminal(&mut pty, 1, child_pid);

    pty.update_and_report_cwds();
    let first = collect_process_info_reports(&screen_rx);
    assert_eq!(first.len(), 1, "the first tick tells Screen what it found");

    // nothing changed, and the pane produced no output
    pty.update_and_report_cwds();
    pty.update_and_report_cwds();
    assert!(
        collect_process_info_reports(&screen_rx).is_empty(),
        "an unchanged map should not be sent again"
    );

    // the pane runs something new
    mock.set_foreground_cmd(child_pid, vec!["vim".into()]);
    pty.pane_activity_flags
        .get(&1)
        .unwrap()
        .store(true, Ordering::Relaxed);
    pty.update_and_report_cwds();
    let after_change = collect_process_info_reports(&screen_rx);
    assert_eq!(
        after_change.len(),
        1,
        "a changed map should reach Screen: {:?}",
        after_change
    );
    assert_eq!(
        after_change[0]
            .get(&1)
            .and_then(|info| info.command.clone()),
        Some(vec!["vim".to_owned()])
    );
}

/// A pty with nothing wired up, for testing state it keeps for itself.
fn make_bare_pty() -> Pty {
    let bus: Bus<PtyInstruction> = Bus::empty().should_silently_fail();
    Pty::new(bus, false, None, None, None, None, None, None)
}

/// A fingerprint that stands for "nothing that gets serialized has changed", so that these tests
/// exercise the dirty/latch arms of `should_serialize_layout` on their own.
const UNCHANGED: u64 = 7;

#[test]
fn the_first_tick_writes_the_cache_even_though_the_session_is_clean() {
    // a session that never diverges from its layout would otherwise never write a resurrection
    // cache at all, and could not be resurrected
    let mut pty = make_bare_pty();
    assert!(
        pty.should_serialize_layout(false, UNCHANGED),
        "the base shape has to reach disk once"
    );
}

#[test]
fn a_clean_session_stops_writing_the_cache() {
    let mut pty = make_bare_pty();
    pty.should_serialize_layout(false, UNCHANGED); // the first tick, which always writes
    for tick in 0..5 {
        assert!(
            !pty.should_serialize_layout(false, UNCHANGED),
            "nothing changed on tick {}, so there is nothing to write",
            tick
        );
    }
}

#[test]
fn a_dirty_session_writes_the_cache_every_tick() {
    let mut pty = make_bare_pty();
    pty.should_serialize_layout(false, UNCHANGED);
    for tick in 0..5 {
        assert!(
            pty.should_serialize_layout(true, UNCHANGED),
            "the session is still diverged on tick {}",
            tick
        );
    }
}

#[test]
fn returning_to_the_base_shape_rewrites_the_cache_once() {
    // the trap the latch exists for: a session that opens a pane and closes it again is clean, and
    // the cache on disk still describes the pane that is gone. Without the transition write, the
    // next resurrection hands that pane back.
    let mut pty = make_bare_pty();
    pty.should_serialize_layout(false, UNCHANGED); // the first tick
    assert!(
        pty.should_serialize_layout(true, UNCHANGED),
        "a pane was opened"
    );
    assert!(
        pty.should_serialize_layout(false, UNCHANGED),
        "the pane was closed - the diverged cache has to be overwritten"
    );
    assert!(
        !pty.should_serialize_layout(false, UNCHANGED),
        "and only once: the cache already matches"
    );
}

#[test]
fn a_clean_session_writes_the_cache_when_a_serialized_attribute_changes() {
    // the regression this guards: a session that stays in the shape of its layout is never dirty,
    // so before the fingerprint it wrote its cache once and then went silent forever - and a pane
    // renamed after that point was lost to the next resurrection.
    let mut pty = make_bare_pty();
    pty.should_serialize_layout(false, UNCHANGED); // the first tick
    assert!(
        !pty.should_serialize_layout(false, UNCHANGED),
        "still clean and still identical"
    );
    assert!(
        pty.should_serialize_layout(false, UNCHANGED + 1),
        "the session is clean, but something it serializes is not what is on disk"
    );
    assert!(
        !pty.should_serialize_layout(false, UNCHANGED + 1),
        "and only once: the cache now matches again"
    );
}

#[test]
fn a_serialized_attribute_that_changes_back_writes_both_times() {
    // a fingerprint the cache has seen before is not the fingerprint the cache HOLDS: renaming a
    // pane and renaming it back has to reach disk twice, or the cache keeps the middle name.
    let mut pty = make_bare_pty();
    pty.should_serialize_layout(false, UNCHANGED);
    assert!(pty.should_serialize_layout(false, UNCHANGED + 1), "renamed");
    assert!(
        pty.should_serialize_layout(false, UNCHANGED),
        "renamed back - the cache holds the middle name and has to be overwritten"
    );
}

#[test]
fn a_pane_whose_identity_is_known_is_never_walked_again() {
    let probe = AgentEnvProbe {
        child_pid: 42,
        found: true,
        ticks_since_probe: 10_000,
        live_session: None,
    };
    assert!(!probe.needs_another_walk(42));
}

#[test]
fn a_new_process_in_the_pane_is_a_new_question() {
    // the identity belongs to the program, so replacing the program invalidates it even when the
    // last walk answered
    let probe = AgentEnvProbe {
        child_pid: 42,
        found: true,
        ticks_since_probe: 0,
        live_session: None,
    };
    assert!(probe.needs_another_walk(43));
}

#[test]
fn a_pane_with_no_identity_yet_is_retried_but_not_every_tick() {
    let mut probe = AgentEnvProbe {
        child_pid: 42,
        found: false,
        ticks_since_probe: 0,
        live_session: None,
    };
    assert!(!probe.needs_another_walk(42));
    probe.ticks_since_probe = AGENT_ENV_RETRY_TICKS - 1;
    assert!(!probe.needs_another_walk(42));
    probe.ticks_since_probe = AGENT_ENV_RETRY_TICKS;
    assert!(probe.needs_another_walk(42));
}

fn pane_running(terminal_id: u32, run: Option<Run>) -> PaneLayoutMetadata {
    PaneLayoutMetadata::new(
        PaneId::Terminal(terminal_id),
        Default::default(),
        false,
        run,
        None,
        false,
        None,
        vec![],
        None,
        None,
        None,
        None,
    )
}

#[test]
fn a_command_pane_is_serialized_as_its_own_command_not_its_newest_child() {
    // an agent run as a command pane IS the pane's process, and its children are its MCP servers
    // and tool shells - the newest of them is not what the pane runs
    let mock = MockOsApi::new();
    mock.set_cmd(100, vec!["claude".into(), "--continue".into()]);
    mock.set_foreground_cmd(100, vec!["sleep".into(), "30".into()]);
    let (mut pty, _rx) = make_pty_with_plugin_receiver(mock);
    pty.id_to_child_pid.insert(1, 100);
    let mut meta = SessionLayoutMetadata::default();
    meta.add_tab(
        "tab1".to_owned(),
        true,
        false,
        vec![pane_running(
            1,
            Some(Run::Command(RunCommand::new(PathBuf::from("claude")))),
        )],
        vec![],
    );

    pty.populate_session_layout_metadata(&mut meta);

    let Some(Run::Command(run_command)) = meta.terminal_run(1) else {
        panic!("expected a command, got {:?}", meta.terminal_run(1));
    };
    assert_eq!(run_command.command, PathBuf::from("claude"));
    assert_eq!(run_command.args, vec!["--continue".to_owned()]);
}

#[test]
fn a_shell_pane_is_still_serialized_as_the_job_it_runs() {
    let mock = MockOsApi::new();
    mock.set_cmd(200, vec!["/bin/bash".into()]);
    mock.set_foreground_cmd(200, vec!["htop".into()]);
    let (mut pty, _rx) = make_pty_with_plugin_receiver(mock);
    pty.id_to_child_pid.insert(2, 200);
    let mut meta = SessionLayoutMetadata::default();
    meta.add_tab(
        "tab1".to_owned(),
        true,
        false,
        vec![pane_running(2, None)],
        vec![],
    );

    pty.populate_session_layout_metadata(&mut meta);

    let Some(Run::Command(run_command)) = meta.terminal_run(2) else {
        panic!("expected a command, got {:?}", meta.terminal_run(2));
    };
    assert_eq!(run_command.command, PathBuf::from("htop"));
}

#[test]
fn a_command_pane_is_listed_as_its_own_command_not_its_newest_child() {
    // the live pane list asks the same question serialization does, and must get the same answer
    let mock = MockOsApi::new();
    mock.set_cmd(100, vec!["claude".into(), "--continue".into()]);
    mock.set_foreground_cmd(100, vec!["sleep".into(), "30".into()]);
    let (mut pty, rx) = make_pty_with_plugin_receiver(mock);
    let (terminal_id, _) = pty
        .spawn_terminal(
            Some(TerminalAction::RunCommand(RunCommand::new(PathBuf::from(
                "claude",
            )))),
            ClientTabIndexOrPaneId::TabIndex(0),
        )
        .expect("the mock spawns a terminal");
    let _ = collect_command_changed_events(&rx);
    pty.pane_activity_flags
        .get(&terminal_id)
        .unwrap()
        .store(true, Ordering::Relaxed);

    pty.update_and_report_cwds();

    let claude = vec!["claude".to_owned(), "--continue".to_owned()];
    assert_eq!(pty.reported_command_for(terminal_id), Some(&claude));
    let events = collect_command_changed_events(&rx);
    assert_eq!(events, vec![(PaneId::Terminal(terminal_id), claude, true)]);
}

#[test]
fn a_shell_pane_is_still_listed_as_the_job_it_runs() {
    let mock = MockOsApi::new();
    mock.set_cmd(100, vec!["/bin/bash".into()]);
    mock.set_foreground_cmd(100, vec!["htop".into()]);
    let (mut pty, _rx) = make_pty_with_plugin_receiver(mock);
    let (terminal_id, _) = pty
        .spawn_terminal(None, ClientTabIndexOrPaneId::TabIndex(0))
        .expect("the mock spawns a terminal");
    pty.pane_activity_flags
        .get(&terminal_id)
        .unwrap()
        .store(true, Ordering::Relaxed);

    pty.update_and_report_cwds();

    assert_eq!(
        pty.reported_command_for(terminal_id),
        Some(&vec!["htop".to_owned()])
    );
}

#[test]
fn a_held_command_pane_stays_a_command_pane_until_it_closes() {
    let mock = MockOsApi::new();
    let (mut pty, _rx) = make_pty_with_plugin_receiver(mock);
    let held_command = RunCommand {
        command: PathBuf::from("claude"),
        hold_on_close: true,
        ..Default::default()
    };
    let (terminal_id, _) = pty
        .spawn_terminal(
            Some(TerminalAction::RunCommand(held_command)),
            ClientTabIndexOrPaneId::TabIndex(0),
        )
        .expect("the mock spawns a terminal");
    assert!(pty.command_terminal_ids.contains(&terminal_id));

    // its command exits and the pane waits to be re-run: still a command pane
    pty.forget_child_pid(terminal_id);
    assert!(pty.command_terminal_ids.contains(&terminal_id));

    pty.close_pane(PaneId::Terminal(terminal_id))
        .expect("the mock closes the pane");
    assert!(!pty.command_terminal_ids.contains(&terminal_id));
}

fn write_claude_record(dir: &std::path::Path, pid: u32, session_id: &str) {
    std::fs::write(
        dir.join(format!("{}.json", pid)),
        format!(r#"{{"pid":{},"sessionId":"{}"}}"#, pid, session_id),
    )
    .unwrap();
}

#[test]
fn a_known_claude_follows_its_session_without_another_walk() {
    // `/clear` moves claude to another session in the same process: the pid the walk was keyed
    // on does not change, so only re-reading claude's own record can see it
    let dir = tempfile::tempdir().unwrap();
    write_claude_record(dir.path(), 77, "first-session");
    let probe = AgentEnvProbe {
        child_pid: 42,
        found: true,
        ticks_since_probe: 0,
        live_session: Some(LiveSession::ClaudeRecord {
            pid: 77,
            dir: dir.path().to_path_buf(),
        }),
    };
    assert!(!probe.needs_another_walk(42));
    assert_eq!(probe.live_session_id().as_deref(), Some("first-session"));
    write_claude_record(dir.path(), 77, "second-session");
    assert_eq!(probe.live_session_id().as_deref(), Some("second-session"));
    std::fs::remove_file(dir.path().join("77.json")).unwrap();
    assert_eq!(probe.live_session_id(), None);
}

#[test]
fn a_pane_with_no_live_session_reads_none() {
    let dir = tempfile::tempdir().unwrap();
    write_claude_record(dir.path(), 77, "first-session");
    let probe = AgentEnvProbe {
        child_pid: 42,
        found: true,
        ticks_since_probe: 0,
        live_session: None,
    };
    assert_eq!(probe.live_session_id(), None);
}

fn agent_pane_meta(terminal_id: u32, command: &str, args: &[&str]) -> SessionLayoutMetadata {
    let mut run_command = RunCommand::new(PathBuf::from(command));
    run_command.args = args.iter().map(|arg| (*arg).to_owned()).collect();
    let mut meta = SessionLayoutMetadata::default();
    meta.add_tab(
        "tab1".to_owned(),
        true,
        false,
        vec![pane_running(terminal_id, Some(Run::Command(run_command)))],
        vec![],
    );
    meta
}

/// The agent's own `OSC 1337` report is the first thing serialization believes: a claude pane
/// is recorded with the id it reported, in place of its `--continue`.
#[test]
fn an_osc_reported_session_is_what_a_pane_is_recorded_with() {
    let mock = MockOsApi::new();
    mock.set_cmd(100, vec!["claude".into(), "--continue".into()]);
    let (mut pty, _rx) = make_pty_with_plugin_receiver(mock);
    pty.resurrect_commands = ResurrectCommands::Agents;
    pty.id_to_child_pid.insert(1, 100);
    pty.note_agent_session_from_osc(1, "osc-session".to_owned());
    let mut meta = agent_pane_meta(1, "claude", &["--continue"]);

    pty.populate_session_layout_metadata(&mut meta);

    let Some(Run::Command(run_command)) = meta.terminal_run(1) else {
        panic!("expected a command, got {:?}", meta.terminal_run(1));
    };
    assert_eq!(
        run_command.args,
        vec!["--resume".to_owned(), "osc-session".to_owned()]
    );
}

/// With no report, no record and no open file, a `--continue` the operator typed is recorded as
/// the picker: never the newest session.
#[test]
fn with_nothing_found_a_typed_latest_flag_is_recorded_as_the_picker() {
    let mock = MockOsApi::new();
    mock.set_cmd(100, vec!["claude".into(), "--continue".into()]);
    let (mut pty, _rx) = make_pty_with_plugin_receiver(mock);
    pty.resurrect_commands = ResurrectCommands::Agents;
    // a pid no process has: the walk finds nothing under it
    pty.id_to_child_pid.insert(1, u32::MAX - 7);
    let mut meta = agent_pane_meta(1, "claude", &["--continue"]);

    pty.populate_session_layout_metadata(&mut meta);

    let Some(Run::Command(run_command)) = meta.terminal_run(1) else {
        panic!("expected a command, got {:?}", meta.terminal_run(1));
    };
    assert_eq!(run_command.args, vec!["--resume".to_owned()]);
    assert!(pty.resume_misses.contains_key(&1));
}

#[test]
fn an_empty_osc_report_clears_the_pane_and_a_closed_pane_loses_its_report() {
    let (mut pty, _rx) = make_pty_with_plugin_receiver(MockOsApi::new());
    pty.id_to_child_pid.insert(1, 100);
    pty.id_to_child_pid.insert(2, 200);
    pty.note_agent_session_from_osc(1, "one".to_owned());
    pty.note_agent_session_from_osc(2, "two".to_owned());
    assert_eq!(pty.osc_agent_session(1), Some("one"));
    pty.note_agent_session_from_osc(1, String::new());
    assert_eq!(pty.osc_agent_session(1), None);
    pty.id_to_child_pid.remove(&2);
    pty.expire_osc_agent_sessions();
    assert_eq!(pty.osc_agent_session(2), None);
}

/// A report outlives a tick on which the agent is not yet the pane's command - the hook fires at
/// start-up - but not the agent: once the pane has run an agent and is back at its shell, the
/// report is gone. A respawned pane is a new program and loses it too.
#[test]
fn an_osc_report_lasts_as_long_as_the_agent_that_made_it() {
    let mut report = OscAgentSession {
        session_id: "s".to_owned(),
        child_pid: Some(100),
        seen_agent: false,
    };
    assert!(
        report.still_holds(Some(100), false),
        "before the tick sees the agent"
    );
    assert!(report.still_holds(Some(100), true));
    assert!(report.seen_agent);
    assert!(!report.still_holds(Some(100), false), "the agent exited");
    let mut report = OscAgentSession {
        session_id: "s".to_owned(),
        child_pid: Some(100),
        seen_agent: true,
    };
    assert!(
        !report.still_holds(Some(101), true),
        "the pane was respawned"
    );
    assert!(
        !report.still_holds(None, true),
        "the pane's process is gone"
    );
}

/// The live list believes the agent's own report too, and says so: `session_source: osc`. Once
/// the agent exits and the pane is back at its shell, the report is gone.
#[test]
fn the_live_list_reports_an_osc_session_while_its_agent_runs() {
    let mock = MockOsApi::new();
    let child_pid = 100;
    mock.set_cmd(child_pid, vec!["claude".into(), "--continue".into()]);
    let (mut pty, screen_rx) = make_pty_with_screen_receiver(mock.clone());
    set_active_terminal(&mut pty, 1, child_pid);
    pty.note_agent_session_from_osc(1, "osc-session".to_owned());

    pty.update_and_report_cwds();
    let reports = collect_process_info_reports(&screen_rx);
    let info = &reports.last().expect("a report")[&1];
    assert_eq!(info.osc_agent_session.as_deref(), Some("osc-session"));

    // claude exits; the pane's process is its shell again
    mock.set_cmd(child_pid, vec!["/bin/bash".into()]);
    pty.pane_activity_flags
        .get(&1)
        .unwrap()
        .store(true, Ordering::Relaxed);
    pty.update_and_report_cwds();
    assert_eq!(pty.osc_agent_session(1), None);
}

/// A real server's pty thread can hear from the thread that reaped a held pane's child. Its bus
/// once had no sender to itself, so this message was dropped and the held pane kept its dead pid.
#[test]
fn the_pty_threads_own_bus_can_tell_it_a_child_was_reaped() {
    let mock = MockOsApi::new();
    let (bus, receivers) = real_pty_bus(mock.clone());
    let mut pty = Pty::new(bus, false, None, None, None, None, None, None);
    let held_command = RunCommand {
        command: PathBuf::from("/bin/does-not-matter"),
        hold_on_close: true,
        ..Default::default()
    };
    let _ = pty
        .spawn_terminal(
            Some(TerminalAction::RunCommand(held_command)),
            ClientTabIndexOrPaneId::TabIndex(0),
        )
        .expect("the mock spawns a terminal");

    mock.exit_last_spawned_pane(PaneId::Terminal(1), Some(0));

    let mut forgotten = vec![];
    while let Ok((instruction, _)) = receivers.pty.try_recv() {
        if let PtyInstruction::ChildProcessExited(terminal_id) = instruction {
            forgotten.push(terminal_id);
        }
    }
    assert_eq!(
        forgotten,
        vec![1],
        "the pty thread's own bus should carry the reaper's message back to it"
    );
}

fn command_pane(terminal_id: u32, argv: &[&str]) -> PaneLayoutMetadata {
    let mut run_command = RunCommand::new(PathBuf::from(argv[0]));
    run_command.args = argv[1..].iter().map(|arg| (*arg).to_owned()).collect();
    pane_running(terminal_id, Some(Run::Command(run_command)))
}

fn two_command_panes_meta() -> SessionLayoutMetadata {
    let mut meta = SessionLayoutMetadata::default();
    meta.add_tab(
        "tab1".to_owned(),
        true,
        false,
        vec![
            command_pane(1, &["sh", "-c", "sleep 1"]),
            command_pane(2, &["sleep", "999"]),
        ],
        vec![],
    );
    meta
}

/// A held pane's pty fd closed with its process, and the next pane's pty may be handed the same
/// number. Asked about that fd, the OS answers for the other pane. Once the pid is forgotten the
/// held pane is not asked at all, and it is serialized as its own command.
#[test]
fn a_held_pane_is_serialized_as_its_own_command_not_the_pane_that_took_its_fd() {
    let mock = MockOsApi::new();
    mock.set_cmd(100, vec!["sh".into(), "-c".into(), "sleep 1".into()]);
    mock.set_cmd(200, vec!["sleep".into(), "999".into()]);
    let (mut pty, _rx) = make_pty_with_plugin_receiver(mock.clone());
    pty.id_to_child_pid.insert(1, 100);
    pty.id_to_child_pid.insert(2, 200);
    pty.command_terminal_ids.insert(1);
    pty.command_terminal_ids.insert(2);
    // pane 1's command exits; pane 2's pty reuses its fd number
    mock.set_terminal_foreground(1, vec!["sleep".into(), "999".into()]);

    // the leak, while the pid is still held
    let mut meta = two_command_panes_meta();
    pty.populate_session_layout_metadata(&mut meta);
    let Some(Run::Command(leaked)) = meta.terminal_run(1) else {
        panic!("expected a command, got {:?}", meta.terminal_run(1));
    };
    assert_eq!(
        leaked.command,
        PathBuf::from("sleep"),
        "the leak this guards"
    );

    pty.forget_child_pid(1);
    assert!(
        !mock.knows_terminal_fd(1),
        "the held pane's fd is forgotten with its pid"
    );

    let mut meta = two_command_panes_meta();
    pty.populate_session_layout_metadata(&mut meta);
    let Some(Run::Command(own)) = meta.terminal_run(1) else {
        panic!("expected a command, got {:?}", meta.terminal_run(1));
    };
    assert_eq!(own.command, PathBuf::from("sh"));
    assert_eq!(own.args, vec!["-c".to_owned(), "sleep 1".to_owned()]);
    let Some(Run::Command(other)) = meta.terminal_run(2) else {
        panic!("expected a command, got {:?}", meta.terminal_run(2));
    };
    assert_eq!(other.command, PathBuf::from("sleep"));
}

/// A held agent pane keeps the session its agent last reported, and serializes with it.
#[test]
fn a_held_agent_pane_is_recorded_with_the_session_it_reported() {
    let mock = MockOsApi::new();
    mock.set_cmd(100, vec!["claude".into(), "--continue".into()]);
    let (mut pty, _rx) = make_pty_with_plugin_receiver(mock);
    pty.resurrect_commands = ResurrectCommands::Agents;
    pty.id_to_child_pid.insert(1, 100);
    pty.note_agent_session_from_osc(1, "osc-held".to_owned());

    pty.forget_child_pid(1);
    pty.expire_osc_agent_sessions();
    let mut meta = agent_pane_meta(1, "claude", &["--continue"]);
    pty.populate_session_layout_metadata(&mut meta);

    let Some(Run::Command(run_command)) = meta.terminal_run(1) else {
        panic!("expected a command, got {:?}", meta.terminal_run(1));
    };
    assert_eq!(
        run_command.args,
        vec!["--resume".to_owned(), "osc-held".to_owned()]
    );
    assert!(!pty.resume_misses.contains_key(&1));
}

/// The id the last tick found - claude's own record, here - is frozen with where it came from.
#[test]
fn a_held_agent_pane_keeps_the_session_the_tick_found_and_its_source() {
    let mock = MockOsApi::new();
    let (mut pty, _rx) = make_pty_with_plugin_receiver(mock);
    pty.resurrect_commands = ResurrectCommands::Agents;
    pty.id_to_child_pid.insert(1, 100);
    pty.terminal_cmds.insert(1, vec!["claude".into()]);
    let harness = agent_detect::harness_for_command("claude").unwrap();
    let mut found = BTreeMap::new();
    found.insert(claude_identity_key(harness), "record-held".to_owned());
    found.insert(AGENT_KIND.to_owned(), "claude".to_owned());
    pty.terminal_agent_envs.insert(1, found);
    pty.agent_env_probes.insert(
        1,
        AgentEnvProbe {
            child_pid: 100,
            found: true,
            ticks_since_probe: 0,
            live_session: Some(LiveSession::ClaudeRecord {
                pid: 100,
                dir: PathBuf::from("/nonexistent"),
            }),
        },
    );

    pty.forget_child_pid(1);
    let held = pty.held_panes.get(&1).expect("the held pane is kept");
    assert_eq!(held.agent_session_id.as_deref(), Some("record-held"));
    assert_eq!(held.session_source(), "record");

    let mut meta = agent_pane_meta(1, "claude", &["--continue"]);
    pty.populate_session_layout_metadata(&mut meta);
    let Some(Run::Command(run_command)) = meta.terminal_run(1) else {
        panic!("expected a command, got {:?}", meta.terminal_run(1));
    };
    assert_eq!(
        run_command.args,
        vec!["--resume".to_owned(), "record-held".to_owned()]
    );
}

/// A held agent pane whose session was never found is recorded with its picker, as before.
#[test]
fn a_held_agent_pane_with_no_session_is_recorded_with_its_picker() {
    let mock = MockOsApi::new();
    let (mut pty, _rx) = make_pty_with_plugin_receiver(mock);
    pty.resurrect_commands = ResurrectCommands::Agents;
    pty.id_to_child_pid.insert(1, 100);

    pty.forget_child_pid(1);
    let mut meta = agent_pane_meta(1, "claude", &["--continue"]);
    pty.populate_session_layout_metadata(&mut meta);

    let Some(Run::Command(run_command)) = meta.terminal_run(1) else {
        panic!("expected a command, got {:?}", meta.terminal_run(1));
    };
    assert_eq!(run_command.args, vec!["--resume".to_owned()]);
    assert!(pty.resume_misses.contains_key(&1));
}

/// A held pane is still in the pane list, with no pid and no command - its process is gone -
/// but with its cwd and its agent's session.
#[test]
fn a_held_pane_is_listed_with_no_pid_and_no_command() {
    let mock = MockOsApi::new();
    mock.set_cmd(100, vec!["claude".into()]);
    mock.set_cwd(100, PathBuf::from("/work"));
    let (mut pty, screen_rx) = make_pty_with_screen_receiver(mock);
    set_active_terminal(&mut pty, 1, 100);
    pty.note_agent_session_from_osc(1, "osc-held".to_owned());
    pty.update_and_report_cwds();
    let reports = collect_process_info_reports(&screen_rx);
    assert_eq!(reports.last().expect("a report")[&1].pid, Some(100));

    pty.forget_child_pid(1);
    pty.update_and_report_cwds();

    let reports = collect_process_info_reports(&screen_rx);
    let info = &reports.last().expect("a report")[&1];
    assert_eq!(info.pid, None);
    assert_eq!(info.command, None);
    assert_eq!(info.cwd, Some(PathBuf::from("/work")));
    assert_eq!(info.osc_agent_session.as_deref(), Some("osc-held"));

    // closed, it is gone from the list
    pty.close_pane(PaneId::Terminal(1)).expect("TEST");
    pty.update_and_report_cwds();
    let reports = collect_process_info_reports(&screen_rx);
    assert!(!reports.last().expect("a report").contains_key(&1));
}
