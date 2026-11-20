use super::*;
use crate::os_input_output::ServerOsApi;
use crate::plugins::PluginInstruction;
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

#[derive(Clone)]
struct MockOsApi {
    cwds: Arc<Mutex<HashMap<u32, PathBuf>>>,
    cmds: Arc<Mutex<HashMap<u32, Vec<String>>>>,
    cmds_by_ppid: Arc<Mutex<HashMap<String, Vec<String>>>>,
}

impl MockOsApi {
    fn new() -> Self {
        MockOsApi {
            cwds: Arc::new(Mutex::new(HashMap::new())),
            cmds: Arc::new(Mutex::new(HashMap::new())),
            cmds_by_ppid: Arc::new(Mutex::new(HashMap::new())),
        }
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
        _pane_env: &crate::os_input_output::PaneEnv,
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
    fn register_client(
        &mut self,
        _: ClientId,
        _: &IpcReceiverWithContext<ClientToServerMsg>,
    ) -> anyhow::Result<()> {
        unimplemented!()
    }
    fn register_client_with_reply(
        &mut self,
        _: ClientId,
        _: LocalSocketStream,
    ) -> anyhow::Result<()> {
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
    fn write_to_file(&mut self, _: String, _: Option<String>) -> anyhow::Result<()> {
        Ok(())
    }
    fn re_run_command_in_terminal(
        &self,
        _: u32,
        _: RunCommand,
        _: Box<dyn Fn(PaneId, Option<i32>, RunCommand) + Send>,
        _pane_env: &crate::os_input_output::PaneEnv,
    ) -> anyhow::Result<(Box<dyn AsyncReader>, Option<u32>)> {
        unimplemented!()
    }
    fn clear_terminal_id(&self, _: u32) -> anyhow::Result<()> {
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
    let pty = Pty::new(bus, false, None, None, Default::default());
    (pty, plugin_rx)
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
    let pty = Pty::new(bus, false, None, None, Default::default(), None, None, None);
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

fn session_with_terminal_pane(terminal_id: u32) -> SessionLayoutMetadata {
    let mut meta = SessionLayoutMetadata::default();
    let pane = crate::session_layout_metadata::PaneLayoutMetadata::new(
        PaneId::Terminal(terminal_id),
        zellij_utils::pane_size::PaneGeom::default(),
        false,
        None,
        None,
        false,
        None,
        vec![],
        None,
        None,
        Default::default(),
        None,
        None,
    );
    meta.add_tab("tab1".to_string(), true, false, vec![pane], vec![]);
    meta
}

fn serialized_run_and_cwd(pty: &mut Pty, terminal_id: u32) -> (Option<Run>, Option<PathBuf>) {
    let mut meta = session_with_terminal_pane(terminal_id);
    pty.populate_session_layout_metadata(&mut meta);
    let manifest: session_serialization::GlobalLayoutManifest = meta.into();
    let global_cwd = manifest.global_cwd;
    let pane = manifest
        .tabs
        .into_iter()
        .next()
        .unwrap()
        .1
        .tiled_panes
        .into_iter()
        .next()
        .unwrap();
    let cwd = match (global_cwd, pane.cwd) {
        (Some(global_cwd), Some(cwd)) => Some(global_cwd.join(cwd)),
        (None, cwd) => cwd,
        (global_cwd, None) => global_cwd,
    };
    (pane.run, cwd)
}

fn run_command(command: &str, args: Vec<&str>) -> Option<Run> {
    let mut run_command = RunCommand::new(PathBuf::from(command));
    run_command.args = args.into_iter().map(|a| a.to_string()).collect();
    Some(Run::Command(run_command))
}

fn resurrect_command(command: &str, args: Vec<&str>, cwd: Option<&str>) -> ResurrectCommand {
    ResurrectCommand {
        command: PathBuf::from(command),
        args: args.into_iter().map(|a| a.to_string()).collect(),
        cwd: cwd.map(PathBuf::from),
    }
}

#[test]
fn resurrect_command_overrides_detected_command_and_cwd() {
    let mock = MockOsApi::new();
    let child_pid = 100;
    mock.set_foreground_cmd(child_pid, vec!["htop".into()]);
    mock.set_cwd(child_pid, PathBuf::from("/tmp/detected"));
    let (mut pty, _rx) = make_pty_with_plugin_receiver(mock);
    set_active_terminal(&mut pty, 1, child_pid);

    pty.set_resurrect_command(
        1,
        Some(resurrect_command(
            "opencode",
            vec!["-s", "ses_abc"],
            Some("/tmp/resurrected"),
        )),
    )
    .unwrap();

    let (run, cwd) = serialized_run_and_cwd(&mut pty, 1);
    assert_eq!(run, run_command("opencode", vec!["-s", "ses_abc"]));
    assert_eq!(cwd, Some(PathBuf::from("/tmp/resurrected")));
}

#[test]
fn resurrect_command_without_cwd_keeps_detected_cwd() {
    let mock = MockOsApi::new();
    let child_pid = 100;
    mock.set_foreground_cmd(child_pid, vec!["htop".into()]);
    mock.set_cwd(child_pid, PathBuf::from("/tmp/detected"));
    let (mut pty, _rx) = make_pty_with_plugin_receiver(mock);
    set_active_terminal(&mut pty, 1, child_pid);

    pty.set_resurrect_command(1, Some(resurrect_command("opencode", vec![], None)))
        .unwrap();

    let (run, cwd) = serialized_run_and_cwd(&mut pty, 1);
    assert_eq!(run, run_command("opencode", vec![]));
    assert_eq!(cwd, Some(PathBuf::from("/tmp/detected")));
}

#[test]
fn cleared_resurrect_command_falls_back_to_detected_command() {
    let mock = MockOsApi::new();
    let child_pid = 100;
    mock.set_foreground_cmd(child_pid, vec!["htop".into()]);
    let (mut pty, _rx) = make_pty_with_plugin_receiver(mock);
    set_active_terminal(&mut pty, 1, child_pid);

    pty.set_resurrect_command(1, Some(resurrect_command("opencode", vec![], None)))
        .unwrap();
    pty.set_resurrect_command(1, None).unwrap();

    let (run, _cwd) = serialized_run_and_cwd(&mut pty, 1);
    assert_eq!(run, run_command("htop", vec![]));
}

#[test]
fn resurrect_command_persists_after_foreground_command_changes() {
    let mock = MockOsApi::new();
    let child_pid = 100;
    mock.set_foreground_cmd(child_pid, vec!["htop".into()]);
    let (mut pty, _rx) = make_pty_with_plugin_receiver(mock.clone());
    set_active_terminal(&mut pty, 1, child_pid);

    pty.set_resurrect_command(1, Some(resurrect_command("opencode", vec![], None)))
        .unwrap();
    mock.set_foreground_cmd(child_pid, vec!["top".into()]);

    let (run, _cwd) = serialized_run_and_cwd(&mut pty, 1);
    assert_eq!(run, run_command("opencode", vec![]));
}

#[test]
fn closing_pane_removes_resurrect_command() {
    let mock = MockOsApi::new();
    let child_pid = 100;
    let (mut pty, _rx) = make_pty_with_plugin_receiver(mock);
    set_active_terminal(&mut pty, 1, child_pid);

    pty.set_resurrect_command(1, Some(resurrect_command("opencode", vec![], None)))
        .unwrap();
    pty.close_pane(PaneId::Terminal(1)).unwrap();

    assert!(!pty.resurrect_commands.contains_key(&1));
}

#[test]
fn resurrect_command_for_unknown_terminal_is_rejected() {
    let mock = MockOsApi::new();
    let (mut pty, _rx) = make_pty_with_plugin_receiver(mock);

    let result = pty.set_resurrect_command(7, Some(resurrect_command("opencode", vec![], None)));

    assert!(result.is_err());
    assert!(pty.resurrect_commands.is_empty());
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
            None,
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
    assert!(
        recorder.signals_sent().is_empty(),
        "nothing should have been signalled"
    );
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
    Pty::new(bus, false, None, None, Default::default(), None, None, None)
}

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

