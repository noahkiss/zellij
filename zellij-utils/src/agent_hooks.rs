//! `zellij setup --agent-hooks`: the hook each coding agent needs for the shared agent-session
//! contract, and the file it goes in.
//!
//! The contract has three parts, and a hook does them in this order on EVERY session start - a
//! claude `/clear`, a pi `/new` and a resumed session each start a new id in the same process:
//!
//! 1. **Always**: export `AGENT_SESSION_ID` (the harness's own native session id) and `AGENT_KIND`
//!    (`claude`, `codex`, `pi`, `opencode`) to every command the harness runs, where the harness
//!    has a way to.
//! 2. **Always**: `agent-sessions register --id <id> --kind <kind>`, guarded by
//!    `command -v agent-sessions`, and `agent-sessions unregister --id <id>` from an end hook where
//!    the harness has one. The registry is a per-machine CLI owned outside zellij; a machine
//!    without it skips the step.
//! 3. **Only inside zellij** (`ZELLIJ` set): print `OSC 1337 SetUserVar=agent_session=<base64 id>`
//!    to the pane's terminal. The server keeps it on that pane and prefers it over every other
//!    source when it records the pane's resume command. See FORK.md, "A restored session offers to
//!    resume its agents, and only its agents".
//!
//! Steps 1 and 2 cover sessions outside zellij too, which is why only step 3 is guarded.
//!
//! The harness snippets are print only: the operator installs them, usually through dotfiles,
//! and nothing here edits a harness's settings. The shared script is different. Every hook calls
//! it by path, and a copy left behind by an older build silently lacks the modes the current
//! hooks call, so the binary owns it: `setup --agent-hooks --write` writes it, and `session
//! doctor` rewrites it when it is missing or stale. The strings build for wasm with the rest of
//! the ungated half; the file half is gated.

/// One agent's install instructions.
pub struct AgentHook {
    /// The harness, as `agent_detect` names it.
    pub kind: &'static str,
    /// The file the snippet goes in.
    pub file: &'static str,
    /// What to do with the snippet, in one line.
    pub how: &'static str,
    /// What the snippet does of the three steps, and what it leaves to something else.
    pub covers: &'static str,
    /// The snippet itself, exactly as it goes in the file.
    pub snippet: &'static str,
    /// What is not proven about it, if anything.
    pub caveat: Option<&'static str>,
}

/// The shared hook script `claude` and `codex` call as `sh <script> <kind> <start|end|osc>`.
pub const SESSION_SCRIPT_PATH: &str = "~/.config/zellij/agent-session.sh";

/// [`SESSION_SCRIPT_PATH`] below `HOME`, with no `~`. The hooks name the script by `~`, so it
/// follows `HOME` and nothing else: not `ZELLIJ_CONFIG_DIR`, not `XDG_CONFIG_HOME`.
const SESSION_SCRIPT_UNDER_HOME: &str = ".config/zellij/agent-session.sh";

/// The sha256 of every `SESSION_SCRIPT` an earlier build shipped, oldest first. A copy that
/// matches one of these is an older build's script; a copy that matches none, and is not this
/// build's either, holds local edits. When `SESSION_SCRIPT` changes, append the old hash here:
/// `the_session_script_hash_is_pinned` fails until you do.
#[cfg(not(target_family = "wasm"))]
const EARLIER_SESSION_SCRIPTS: &[&str] = &[
    "f72dce7d091b9ffe196985571f754ca1e17b3d592e3affe31ae28692722ae1fb", // OSC 1337 report only
    "55cc7140a29fb6ba3ed22d70dfe892602ef7026a2f82fb6863650b5cc7ceab2f", // before the state mode
    "6eacd8a12d1a95445db6cc818efc603c5337978d4e8026e46c90caae2a8031d7", // before the context mode
];

/// Both harnesses hand their hook a JSON object on stdin with a `session_id` field. A hook's
/// stdout is the agent's, not the terminal, and a claude hook has no controlling terminal at all -
/// verified with Claude Code 2.1: `ps -o tty=` prints `?` for it - so the OSC step walks up its
/// ancestors to the agent itself, by executable name or argv0, and writes to the agent's terminal
/// device. It stops at the agent: a `codex exec` with no terminal of its own climbed past it, in a
/// test, to the terminal of an unrelated pane further up.
pub const SESSION_SCRIPT: &str = r#"#!/bin/sh
# Agent session hook (zellij setup --agent-hooks).
# Usage: sh agent-session.sh KIND start|end|osc|context
#        sh agent-session.sh KIND state STATE [BLOCKED_KIND]
#   start: register the session in the agent-sessions registry, then report it to zellij
#   end:   unregister it
#   osc:   only report it to zellij
#   context: print live pane context for SessionStart, within 150 milliseconds
#   state: only tell zellij what the agent is doing (OSC 7501): idle, working, done, blocked
#          (with permission, question or auth) or error. For an agent that does not report
#          it itself; claude and pi do.
# Reads the hook's JSON on stdin for its "session_id".
kind=$1
mode=${2:-start}
if [ "$mode" = context ]; then
  [ -n "$ZELLIJ" ] && [ -n "$ZELLIJ_PANE_ID" ] || exit 0
  command -v zellij >/dev/null 2>&1 || exit 0
  zellij action whoami 2>/dev/null &
  context_pid=$!
  (sleep 0.15; kill "$context_pid" 2>/dev/null) </dev/null >/dev/null 2>&1 &
  watchdog_pid=$!
  wait "$context_pid" 2>/dev/null
  kill "$watchdog_pid" 2>/dev/null
  wait "$watchdog_pid" 2>/dev/null
  exit 0
fi
if [ "$mode" = state ]; then
  cat >/dev/null
  case "$3" in idle|working|done|blocked|error) ;; *) exit 0 ;; esac
  report="state=$3:app=$kind"
  case "$3:$4" in blocked:permission|blocked:question|blocked:auth) report="$report:kind=$4" ;; esac
else
  id=$(tr -d '\n' | sed -n 's/.*"session_id"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')
  [ -n "$id" ] || exit 0
fi
case "$mode" in
  start)
    if command -v agent-sessions >/dev/null; then
      agent-sessions register --id "$id" --kind "$kind" >/dev/null 2>&1
    fi
    ;;
  end)
    if command -v agent-sessions >/dev/null; then
      agent-sessions unregister --id "$id" >/dev/null 2>&1
    fi
    exit 0
    ;;
esac
# Only inside zellij: tell the pane which session it holds.
[ -n "$ZELLIJ" ] || exit 0
# A hook may run with no controlling terminal (claude starts it in a session of its own), so
# /dev/tty can fail. Walk up to the agent itself - the nearest ancestor whose executable or
# argv0 is named KIND - and use ITS terminal. Never an ancestor above the agent: a codex run
# with no terminal of its own would otherwise report into whatever pane started it.
p=$$
t=
while [ "${p:-1}" -gt 1 ]; do
  c=$(ps -o comm= -p "$p" 2>/dev/null)
  a=$(ps -o args= -p "$p" 2>/dev/null | awk '{print $1}')
  case "${c##*/} ${a##*/}" in
    "$kind "* | *" $kind") t=$(ps -o tty= -p "$p" 2>/dev/null | tr -d ' '); break ;;
  esac
  p=$(ps -o ppid= -p "$p" 2>/dev/null | tr -d ' ')
done
case "$t" in ''|'?'|'??') exit 0 ;; esac
if [ "$mode" = state ]; then
  { printf '\033]7501;%s\007' "$report" > "/dev/$t"; } 2>/dev/null
  exit 0
fi
b64=$(printf %s "$id" | base64 | tr -d '\n')
{ printf '\033]1337;SetUserVar=agent_session=%s\007' "$b64" > "/dev/$t"; } 2>/dev/null
exit 0
"#;

pub const AGENT_HOOKS: &[AgentHook] = &[
    AgentHook {
        kind: "claude",
        file: "~/.claude/settings.json (or $CLAUDE_CONFIG_DIR/settings.json)",
        how: "merge into the top-level \"hooks\" object; SessionStart fires on start-up, /clear, \
              /resume and compaction",
        covers: "register on SessionStart, unregister on SessionEnd, the zellij report, and live \
                 pane context. Registration is this hook's job. `agent-sessions register` merges \
                 into an existing entry, so bm's SessionStart hook may register the same id again \
                 to add bm_project, in either order. The env export ($CLAUDE_ENV_FILE) lives in \
                 bm's SessionStart hook; do not add it here",
        snippet: r#"{
  "hooks": {
    "SessionStart": [
      {
        "hooks": [
          { "type": "command", "command": "sh ~/.config/zellij/agent-session.sh claude start" },
          { "type": "command", "command": "sh ~/.config/zellij/agent-session.sh claude context" }
        ]
      }
    ],
    "SessionEnd": [
      {
        "hooks": [
          { "type": "command", "command": "sh ~/.config/zellij/agent-session.sh claude end" }
        ]
      }
    ]
  }
}"#,
        caveat: None,
    },
    AgentHook {
        kind: "codex",
        file: "~/.codex/config.toml (or $CODEX_HOME/config.toml)",
        how: "append; codex asks once to trust a new hook",
        covers: "register on SessionStart, unregister on SessionEnd, the zellij report, and \
                 the agent's state (OSC 7501), which codex does not report itself: working on \
                 UserPromptSubmit and PostToolUse, blocked on PermissionRequest, done on Stop. No \
                 env export: codex 0.160 has no way for a hook to set the environment of the \
                 commands it runs",
        snippet: r#"[[hooks.SessionStart]]

[[hooks.SessionStart.hooks]]
type = "command"
command = "sh ~/.config/zellij/agent-session.sh codex start"
timeout = 5

[[hooks.SessionEnd]]

[[hooks.SessionEnd.hooks]]
type = "command"
command = "sh ~/.config/zellij/agent-session.sh codex end"
timeout = 5

[[hooks.UserPromptSubmit]]

[[hooks.UserPromptSubmit.hooks]]
type = "command"
command = "sh ~/.config/zellij/agent-session.sh codex state working"
timeout = 5

[[hooks.PermissionRequest]]

[[hooks.PermissionRequest.hooks]]
type = "command"
command = "sh ~/.config/zellij/agent-session.sh codex state blocked permission"
timeout = 5

[[hooks.PostToolUse]]

[[hooks.PostToolUse.hooks]]
type = "command"
command = "sh ~/.config/zellij/agent-session.sh codex state working"
timeout = 5

[[hooks.Stop]]

[[hooks.Stop.hooks]]
type = "command"
command = "sh ~/.config/zellij/agent-session.sh codex state done"
timeout = 5"#,
        caveat: Some(
            "not proven in an interactive codex: if codex runs its hooks in its app-server \
             daemon, register finds no codex above the hook and the hook has no pane terminal; \
             the open rollout file still answers zellij",
        ),
    },
    AgentHook {
        kind: "opencode",
        file: "~/.config/opencode/plugins/agent-session-env.ts (opencode v1 and v2, runs in the server)",
        how: "create the file; opencode loads every file in that directory into its server. One \
              file serves both versions: v2 calls `setup`, v1 calls `server`, and each ignores the \
              other's key",
        covers: "the env export: AGENT_SESSION_ID and AGENT_KIND on every command the agent runs. \
                 Without it a nested opencode carries its parent's variables and reads as its parent",
        snippet: r#"// Agent session hook (zellij setup --agent-hooks): export AGENT_SESSION_ID and AGENT_KIND to the
// commands opencode runs. One file serves opencode v1 and v2: v2 calls `setup`, v1 calls `server`,
// and each ignores the other's key. Nothing is imported, because neither version can resolve the
// other's plugin package.
export default {
  id: "agent-session-env",
  // v2: the shell hook is not told the session, and opencode sets its own OPENCODE_SESSION_ID only
  // after it, so the tool hook that runs first remembers the session per command and the shell
  // hook picks it up.
  async setup(ctx: any) {
    const pending = new Map<string, string>()
    await ctx.tool.hook("execute.before", (event: any) => {
      const command = event.input?.command
      if (typeof command === "string" && event.sessionID) pending.set(command, event.sessionID)
    })
    await ctx.shell.hook("create.before", (event: any) => {
      const id = pending.get(event.command)
      if (!id) return
      pending.delete(event.command)
      event.env.AGENT_SESSION_ID = id
      event.env.AGENT_KIND = "opencode"
    })
  },
  // v1: `shell.env` runs for every command and is told the session.
  async server() {
    return {
      "shell.env": async (input: any, output: any) => {
        if (!input.sessionID) return
        output.env.AGENT_SESSION_ID = input.sessionID
        output.env.AGENT_KIND = "opencode"
      },
    }
  },
}"#,
        caveat: None,
    },
    AgentHook {
        kind: "opencode",
        file: "~/.config/opencode/plugins/agent-session/tui.ts (opencode v1 and v2)",
        how: "create the file, and beside it package.json: \
              {\"name\": \"agent-session\", \"type\": \"module\", \"exports\": {\"./tui\": \"./tui.ts\"}}. \
              opencode v2 finds a plugin directory in plugins/ by itself; for opencode v1, add \
              \"./plugins/agent-session\" to \"plugin\" in ~/.config/opencode/tui.json. \
              A TUI plugin runs in the TUI, the process in the pane; the v2 server and the v1 \
              server skip the directory",
        covers: "register when the TUI opens a session, unregister when the TUI exits, the \
                 zellij report, and the agent's state (OSC 7501), which opencode does not report \
                 itself: working while the shown session or a subagent runs, blocked on a \
                 permission or a question, error when the session fails, done when it ends. The \
                 env export is the server plugin above; a one-shot `opencode run` has no TUI and \
                 is neither registered nor reported",
        snippet: r#"import { execFile, execFileSync } from "node:child_process"
import { writeFileSync } from "node:fs"

// Agent session hook (zellij setup --agent-hooks). Runs in the TUI, the process in the pane. One
// file serves opencode v1 and v2: v2 calls `setup`, v1 calls `tui`. Nothing is imported from
// opencode, because neither version can resolve the other's plugin package.
const REGISTRY = 'command -v agent-sessions >/dev/null && exec agent-sessions "$@"'

// Register and report the session the TUI shows, each time it changes. Returns the cleanup.
function watch(sessionOf: () => string, changed: (id: string) => void) {
  let current = ""
  const check = () => {
    const id = sessionOf()
    if (!id || id === current) return
    current = id
    changed(id)
    execFile("sh", ["-c", REGISTRY, "sh", "register", "--id", id, "--kind", "opencode"], () => {})
    // Only inside zellij: tell the pane which session it holds.
    if (!process.env.ZELLIJ) return
    const b64 = Buffer.from(id).toString("base64")
    try {
      writeFileSync("/dev/tty", `\x1b]1337;SetUserVar=agent_session=${b64}\x07`)
    } catch {}
  }
  const timer = setInterval(check, 1000)
  check()
  return () => {
    clearInterval(timer)
    if (!current) return
    try {
      execFileSync("sh", ["-c", REGISTRY, "sh", "unregister", "--id", current], { stdio: "ignore" })
    } catch {}
  }
}

// zellij drops a report whose text holds a control character, so flatten it and keep it short.
function text(msg: string) {
  const flat = String(msg ?? "").replace(/[\u0000-\u001f\u007f-\u009f]+/g, " ").trim().slice(0, 400)
  return flat ? `:msg=${Buffer.from(flat).toString("base64")}` : ""
}

type Tracked = {
  ran: boolean
  failed: string
  busy: Set<string>
  asks: Map<string, { session: string; kind: string; msg: string }>
}

// Tell zellij what the agent is doing (OSC 7501), which opencode does not report itself: the
// state of the session the TUI shows, with the subagents under it. `rootOf` names a session's
// top. Every top session heard of is tracked, because a new session starts running before the
// TUI shows it. Blocked beats working, working beats error, and error beats done.
function status(rootOf: (id: string) => string) {
  let shown = ""
  let sent = ""
  const roots = new Map<string, Tracked>()
  const of = (root: string) => {
    let tracked = roots.get(root)
    if (!tracked) {
      tracked = { ran: false, failed: "", busy: new Set(), asks: new Map() }
      roots.set(root, tracked)
      // keep the newest few: one TUI may hear every session of a shared service
      for (const old of roots.keys()) if (roots.size > 32 && old !== shown) roots.delete(old)
    }
    return tracked
  }
  const report = () => {
    const t = shown ? of(shown) : undefined
    const ask = t ? [...t.asks.values()].pop() : undefined
    const next = !t
      ? ""
      : ask
        ? `state=blocked:kind=${ask.kind}${text(ask.msg)}`
        : t.busy.size
          ? "state=working"
          : t.failed
            ? `state=error${text(t.failed)}`
            : t.ran
              ? "state=done"
              : sent && "state=clear"
    if (!next || next === sent) return
    sent = next
    // Only inside zellij: the pane keeps the record, and list-agents shows it.
    if (!process.env.ZELLIJ) return
    try {
      writeFileSync("/dev/tty", `\x1b]7501;${next}:app=opencode\x07`)
    } catch {}
  }
  // Apply a change to the session's top, and report if that is the one shown.
  const change = (session: string, apply: (t: Tracked, root: string) => void) => {
    if (!session) return
    const root = rootOf(session)
    apply(of(root), root)
    if (root === shown) report()
  }
  return {
    // The TUI shows another session. `running` covers one that ran before this plugin started.
    follow(session: string, running: boolean) {
      shown = rootOf(session)
      const t = of(shown)
      if (running && !t.busy.size) {
        t.busy.add(session)
        t.ran = true
      }
      report()
    },
    start: (session: string) =>
      change(session, (t, root) => {
        t.busy.add(session)
        t.ran = true
        if (session === root) t.failed = ""
      }),
    end: (session: string) =>
      change(session, (t) => {
        t.busy.delete(session)
        for (const [id, ask] of t.asks) if (ask.session === session) t.asks.delete(id)
      }),
    // Only the top session's error is the agent's, and a user's abort is not one.
    fail: (session: string, msg: string) =>
      change(session, (t, root) => {
        if (session === root && msg) t.failed = msg
      }),
    ask: (session: string, id: string, kind: string, msg: string) =>
      change(session, (t) => t.asks.set(id, { session, kind, msg })),
    answer(id: string) {
      for (const [root, t] of roots) if (t.asks.delete(id) && root === shown) report()
    },
  }
}

export default {
  id: "agent-session",
  // v2: the route is a function on context.ui.router, and an event's body is its `data`
  setup(context: any) {
    const data = context.data
    const state = status((id) => data.session.root(id))
    const stop = watch(
      () => {
        const route = context.ui.router.current()
        return route?.type === "session" ? route.sessionID : ""
      },
      (id) => state.follow(id, data.session.status(id) === "running"),
    )
    const on = (type: string, handler: (event: any) => void) =>
      data.on(type, (event: any) => handler(event.data))
    const off = [
      on("session.execution.started", (e) => state.start(e.sessionID)),
      on("session.execution.succeeded", (e) => state.end(e.sessionID)),
      on("session.execution.interrupted", (e) => state.end(e.sessionID)),
      on("session.execution.failed", (e) => {
        state.fail(e.sessionID, e.error?.message ?? "error")
        state.end(e.sessionID)
      }),
      on("permission.asked", (e) =>
        state.ask(e.sessionID, e.id, "permission", e.message ?? `${e.action}: ${(e.resources ?? []).join(" ")}`),
      ),
      on("permission.replied", (e) => state.answer(e.requestID)),
      on("form.created", (e) => state.ask(e.form.sessionID, e.form.id, "question", e.form.title)),
      on("form.replied", (e) => state.answer(e.id)),
      on("form.cancelled", (e) => state.answer(e.id)),
    ]
    return () => {
      for (const unsubscribe of off) unsubscribe()
      stop()
    }
  },
  // v1: the route is a property on api.route, and an event's body is its `properties`
  async tui(api: any) {
    const state = status((id) => {
      // walk up to the top session
      for (let depth = 0, parent = api.state.session.get(id)?.parentID; parent && depth < 8; depth++) {
        id = parent
        parent = api.state.session.get(id)?.parentID
      }
      return id
    })
    const stop = watch(
      () => {
        const route = api.route.current
        return route?.name === "session" ? route.params?.sessionID ?? "" : ""
      },
      (id) => state.follow(id, ["busy", "retry"].includes(api.state.session.status(id)?.type)),
    )
    const on = (type: string, handler: (event: any) => void) =>
      api.event.on(type, (event: any) => handler(event.properties))
    const off = [
      on("session.status", (e) => (e.status?.type === "idle" ? state.end(e.sessionID) : state.start(e.sessionID))),
      on("session.error", (e) =>
        state.fail(
          e.sessionID,
          e.error?.name === "MessageAbortedError" ? "" : e.error?.data?.message ?? e.error?.name ?? "error",
        ),
      ),
      on("permission.asked", (e) =>
        state.ask(e.sessionID, e.id, "permission", `${e.permission}: ${(e.patterns ?? []).join(" ")}`),
      ),
      on("permission.replied", (e) => state.answer(e.requestID)),
      on("question.asked", (e) => state.ask(e.sessionID, e.id, "question", e.questions?.[0]?.question ?? "")),
      on("question.replied", (e) => state.answer(e.requestID)),
      on("question.rejected", (e) => state.answer(e.requestID)),
    ]
    api.lifecycle.onDispose(() => {
      for (const unsubscribe of off) unsubscribe()
      stop()
    })
  },
}"#,
        caveat: None,
    },
    AgentHook {
        kind: "pi",
        file: "~/.pi/agent/extensions/agent-session.ts (or $PI_CODING_AGENT_DIR/extensions/)",
        how: "create the file; pi loads every extension in that directory at start-up",
        covers: "the env export (pi's own environment, which every command it runs inherits), \
                 register on session_start, unregister on session_shutdown, and the zellij report",
        snippet: r#"import { execFile, execFileSync } from "node:child_process";
import { writeFileSync } from "node:fs";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

// Agent session hook (zellij setup --agent-hooks).
const REGISTRY = 'command -v agent-sessions >/dev/null && exec agent-sessions "$@"';

export default function agentSession(pi: ExtensionAPI) {
  // start-up, /new, /resume and /fork each start a session
  pi.on("session_start", async (_event, ctx) => {
    const id = ctx.sessionManager.getSessionId();
    if (!id) return;
    process.env.AGENT_SESSION_ID = id;
    process.env.AGENT_KIND = "pi";
    execFile("sh", ["-c", REGISTRY, "sh", "register", "--id", id, "--kind", "pi"], () => {});
    // Only inside zellij: tell the pane which session it holds.
    if (!process.env.ZELLIJ) return;
    const b64 = Buffer.from(id).toString("base64");
    try {
      writeFileSync("/dev/tty", `\x1b]1337;SetUserVar=agent_session=${b64}\x07`);
    } catch {}
  });
  pi.on("session_shutdown", async (_event, ctx) => {
    const id = ctx.sessionManager.getSessionId();
    if (!id) return;
    try {
      execFileSync("sh", ["-c", REGISTRY, "sh", "unregister", "--id", id], { stdio: "ignore" });
    } catch {}
  });
}"#,
        caveat: None,
    },
];

/// Where the shared script lives under `home`.
pub fn session_script_path(home: &std::path::Path) -> std::path::PathBuf {
    home.join(SESSION_SCRIPT_UNDER_HOME)
}

/// What the shared script on disk is, compared with this build's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScriptState {
    /// No file there.
    Missing,
    /// Byte for byte this build's [`SESSION_SCRIPT`].
    Current,
    /// An earlier build's script, unedited.
    Earlier,
    /// Matches no build's script: somebody edited it.
    Edited,
    /// There, and not readable as a file.
    Unreadable(String),
}

#[cfg(not(target_family = "wasm"))]
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{:02x}", byte))
        .collect()
}

/// Compare the script at `path` with this build's.
#[cfg(not(target_family = "wasm"))]
pub fn session_script_state(path: &std::path::Path) -> ScriptState {
    script_state_against(path, EARLIER_SESSION_SCRIPTS)
}

/// [`session_script_state`] against a given list of earlier hashes, so a test can name one.
#[cfg(not(target_family = "wasm"))]
fn script_state_against(path: &std::path::Path, earlier: &[&str]) -> ScriptState {
    match std::fs::read(path) {
        Ok(bytes) if bytes == SESSION_SCRIPT.as_bytes() => ScriptState::Current,
        Ok(bytes) if earlier.contains(&sha256_hex(&bytes).as_str()) => ScriptState::Earlier,
        Ok(_) => ScriptState::Edited,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ScriptState::Missing,
        Err(error) => ScriptState::Unreadable(error.to_string()),
    }
}

/// Write this build's script to `path`, and say what was there before.
///
/// A current file is left untouched, so a second run writes nothing. Otherwise the bytes go to a
/// temporary file in the same directory, which is renamed over the old one: a hook that starts
/// mid-write reads the old script or the new one, never half of either. A symlink is written
/// through, so a dotfiles link keeps pointing where it did. The directory is created if needed.
#[cfg(not(target_family = "wasm"))]
pub fn write_session_script(path: &std::path::Path) -> std::io::Result<ScriptState> {
    use std::io::Write;
    let before = session_script_state(path);
    if before == ScriptState::Current {
        return Ok(before);
    }
    let target = if path.is_symlink() {
        std::fs::canonicalize(path)?
    } else {
        path.to_path_buf()
    };
    let directory = target
        .parent()
        .ok_or_else(|| std::io::Error::other("the script path has no directory"))?;
    std::fs::create_dir_all(directory)?;
    let file_name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temporary = directory.join(format!(".{}.{}.tmp", file_name, std::process::id()));
    let written = std::fs::File::create(&temporary).and_then(|mut file| {
        file.write_all(SESSION_SCRIPT.as_bytes())?;
        file.sync_all()
    });
    if let Err(error) = written.and_then(|_| std::fs::rename(&temporary, &target)) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    Ok(before)
}

/// `setup --agent-hooks --write`: write the script under `HOME`, and say in one line what was done.
#[cfg(not(target_family = "wasm"))]
pub fn write_home_session_script() -> Result<String, String> {
    let home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .ok_or_else(|| {
            String::from("HOME is not set, so there is no ~ to write the script under")
        })?;
    let path = session_script_path(std::path::Path::new(&home));
    let before = write_session_script(&path)
        .map_err(|error| format!("could not write {}: {}", path.display(), error))?;
    Ok(match before {
        ScriptState::Current => format!("{} is already this build's script", path.display()),
        ScriptState::Missing => format!("wrote {}", path.display()),
        ScriptState::Earlier => {
            format!("rewrote {}: it was an older build's script", path.display())
        },
        ScriptState::Edited => format!(
            "rewrote {}: it held local edits, which are gone",
            path.display()
        ),
        ScriptState::Unreadable(why) => {
            format!("rewrote {}: it could not be read ({})", path.display(), why)
        },
    })
}

#[cfg(target_family = "wasm")]
pub fn write_home_session_script() -> Result<String, String> {
    Err(String::from(
        "a plugin cannot write the agent-session script",
    ))
}

/// What `zellij setup --agent-hooks` prints.
pub fn agent_hooks_text() -> String {
    let mut text = String::new();
    text.push_str(
        "# Agent session hooks\n\
         #\n\
         # On every session start each hook:\n\
         #   1. exports AGENT_SESSION_ID and AGENT_KIND to the commands the agent runs\n\
         #   2. runs `agent-sessions register` when that command exists (and unregister at the end)\n\
         #   3. inside zellij only: prints OSC 1337 SetUserVar=agent_session=<base64 id> to the pane\n\
         # zellij keeps the step 3 report on the pane and resumes exactly that session.\n\
         # claude and pi report their own state (OSC 7501) once zellij answers their query; the\n\
         # codex hook and the opencode TUI plugin report it for theirs, and list-agents --json\n\
         # carries it as `state`.\n\
         # `--write` writes the shared hook script, and `zellij session doctor` keeps it current.\n\
         # Nothing else here is written for you: no harness settings file is touched.\n",
    );
    text.push_str(&format!(
        "\n## shared hook script (claude, codex)\n# file: {}\n# `zellij setup --agent-hooks --write` writes it; it needs no execute bit\n\n{}",
        SESSION_SCRIPT_PATH, SESSION_SCRIPT
    ));
    for hook in AGENT_HOOKS {
        text.push_str(&format!(
            "\n## {}\n# file: {}\n# {}\n# covers: {}\n",
            hook.kind, hook.file, hook.how, hook.covers
        ));
        if let Some(caveat) = hook.caveat {
            text.push_str(&format!("# {}\n", caveat));
        }
        text.push('\n');
        text.push_str(hook.snippet);
        text.push('\n');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hook(kind: &str) -> &'static AgentHook {
        // opencode has two: the env export in its server, and the rest in its TUI
        let tui = |hook: &&AgentHook| kind != "opencode" || hook.file.contains("tui.ts");
        AGENT_HOOKS
            .iter()
            .filter(|hook| hook.kind == kind)
            .find(tui)
            .unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn context_hook_is_silent_outside_zellij_and_bounds_a_slow_cli() {
        use std::os::unix::fs::PermissionsExt;
        use std::process::Command;
        use std::time::{Duration, Instant};
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("agent-session.sh");
        std::fs::write(&script, SESSION_SCRIPT).unwrap();
        let cli = dir.path().join("zellij");
        std::fs::write(&cli, "#!/bin/sh\nexec sleep 5\n").unwrap();
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
        let outside = Command::new("sh")
            .arg(&script)
            .args(["claude", "context"])
            .env_remove("ZELLIJ")
            .env_remove("ZELLIJ_PANE_ID")
            .output()
            .unwrap();
        assert!(outside.status.success());
        assert!(outside.stdout.is_empty() && outside.stderr.is_empty());
        let start = Instant::now();
        let inside = Command::new("sh")
            .arg(&script)
            .args(["claude", "context"])
            .env("ZELLIJ", "1")
            .env("ZELLIJ_PANE_ID", "7")
            .env("PATH", format!("{}:/usr/bin:/bin", dir.path().display()))
            .output()
            .unwrap();
        // CI scheduling adds wall time; the watchdog must still interrupt the five-second CLI.
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(inside.status.success());
        assert!(inside.stdout.is_empty() && inside.stderr.is_empty());
        std::fs::write(&cli, "#!/bin/sh\nprintf 'live pane context\\n'\n").unwrap();
        let fast_start = Instant::now();
        let context = Command::new("sh")
            .arg(&script)
            .args(["claude", "context"])
            .env("ZELLIJ", "1")
            .env("ZELLIJ_PANE_ID", "7")
            .env("PATH", format!("{}:/usr/bin:/bin", dir.path().display()))
            .output()
            .unwrap();
        assert!(fast_start.elapsed() < Duration::from_millis(200));
        assert_eq!(
            String::from_utf8(context.stdout).unwrap(),
            "live pane context\n"
        );
    }

    #[test]
    fn claude_session_start_installs_bounded_live_context() {
        let hook: serde_json::Value = serde_json::from_str(AGENT_HOOKS[0].snippet).unwrap();
        let hooks = hook["hooks"]["SessionStart"][0]["hooks"]
            .as_array()
            .unwrap();
        assert!(hooks.iter().any(|hook| hook["command"]
            .as_str()
            .unwrap()
            .ends_with("claude context")));
        assert!(SESSION_SCRIPT.contains("sleep 0.15"));
        assert!(
            SESSION_SCRIPT.contains("[ -n \"$ZELLIJ\" ] && [ -n \"$ZELLIJ_PANE_ID\" ] || exit 0")
        );
    }

    #[test]
    fn every_known_agent_has_a_hook() {
        for harness in crate::agent_detect::HARNESSES {
            assert!(
                AGENT_HOOKS.iter().any(|hook| hook.kind == harness.kind),
                "{} has no hook",
                harness.kind
            );
        }
    }

    /// The register and unregister steps run outside zellij too: only the report is guarded.
    #[test]
    fn the_script_registers_before_it_checks_for_zellij() {
        let register = SESSION_SCRIPT.find("agent-sessions register").unwrap();
        let unregister = SESSION_SCRIPT.find("agent-sessions unregister").unwrap();
        let guard = SESSION_SCRIPT
            .find(r#"[ -n "$ZELLIJ" ] || exit 0"#)
            .unwrap();
        let report = SESSION_SCRIPT.find("SetUserVar=agent_session=").unwrap();
        assert!(register < guard && unregister < guard && guard < report);
        assert!(SESSION_SCRIPT.contains("command -v agent-sessions >/dev/null"));
        assert!(SESSION_SCRIPT.contains(r#"> "/dev/$t""#));
    }

    #[test]
    fn the_in_process_hooks_export_register_and_guard_only_the_report() {
        for kind in ["opencode", "pi"] {
            let snippet = hook(kind).snippet;
            for needed in [
                "\"register\"",
                "\"unregister\"",
                "command -v agent-sessions >/dev/null",
                "SetUserVar=agent_session=",
            ] {
                assert!(snippet.contains(needed), "{} lacks {}", kind, needed);
            }
            let register = snippet.find("\"register\"").unwrap();
            let guard = snippet.find("if (!process.env.ZELLIJ) return").unwrap();
            let report = snippet.find("SetUserVar=agent_session=").unwrap();
            assert!(register < guard && guard < report, "{}", kind);
        }
    }

    /// claude registers through this hook now; only the env export stays in bm's.
    #[test]
    fn the_claude_hook_starts_and_ends_and_says_where_the_rest_lives() {
        let claude = hook("claude");
        let value: serde_json::Value = serde_json::from_str(claude.snippet).unwrap();
        let commands = |event: &str| -> Vec<String> {
            value["hooks"][event][0]["hooks"]
                .as_array()
                .unwrap()
                .iter()
                .map(|hook| hook["command"].as_str().unwrap().to_owned())
                .collect()
        };
        assert_eq!(
            commands("SessionStart"),
            [
                "sh ~/.config/zellij/agent-session.sh claude start",
                "sh ~/.config/zellij/agent-session.sh claude context",
            ]
        );
        assert_eq!(
            commands("SessionEnd"),
            ["sh ~/.config/zellij/agent-session.sh claude end"]
        );
        assert!(!claude.snippet.contains("claude osc"));
        assert!(!claude.snippet.contains("agent-sessions"));
        for needed in [
            "Registration is this hook's job",
            "merges",
            "bm_project",
            "$CLAUDE_ENV_FILE",
        ] {
            assert!(claude.covers.contains(needed), "covers lacks {}", needed);
        }
    }

    /// The modes claude's hooks run, outside zellij: `start` registers and `end` unregisters by
    /// the id in the hook's JSON, and neither prints anything.
    #[cfg(unix)]
    #[test]
    fn start_and_end_register_and_unregister_the_id_from_the_hook_json() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        use std::process::{Command, Stdio};
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("agent-session.sh");
        std::fs::write(&script, SESSION_SCRIPT).unwrap();
        let log = dir.path().join("calls");
        let registry = dir.path().join("agent-sessions");
        std::fs::write(
            &registry,
            format!("#!/bin/sh\necho \"$@\" >> '{}'\n", log.display()),
        )
        .unwrap();
        std::fs::set_permissions(&registry, std::fs::Permissions::from_mode(0o755)).unwrap();
        for mode in ["start", "end"] {
            let mut child = Command::new("sh")
                .arg(&script)
                .args(["claude", mode])
                .env_remove("ZELLIJ")
                .env("PATH", format!("{}:/usr/bin:/bin", dir.path().display()))
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            child
                .stdin
                .take()
                .unwrap()
                .write_all(br#"{"session_id": "abc-123", "hook_event_name": "SessionEnd"}"#)
                .unwrap();
            let output = child.wait_with_output().unwrap();
            assert!(output.status.success(), "{}", mode);
            assert!(
                output.stdout.is_empty() && output.stderr.is_empty(),
                "{}",
                mode
            );
        }
        assert_eq!(
            std::fs::read_to_string(&log).unwrap(),
            "register --id abc-123 --kind claude\nunregister --id abc-123\n"
        );
    }

    /// The hash list is how doctor tells an older build's script from an edited one, and it works
    /// only if every shipped script is in it. Changing `SESSION_SCRIPT` fails this until the old
    /// hash is appended to `EARLIER_SESSION_SCRIPTS` and the new one is pinned here.
    #[test]
    fn the_session_script_hash_is_pinned() {
        let current = sha256_hex(SESSION_SCRIPT.as_bytes());
        assert_eq!(
            current, "969d8d1e183c3510a3b767b83e21088ddedeae922e53f61a44de0727fe148fb7",
            "SESSION_SCRIPT changed: append the old hash to EARLIER_SESSION_SCRIPTS, then pin the \
             new one here"
        );
        assert!(!EARLIER_SESSION_SCRIPTS.contains(&current.as_str()));
    }

    #[test]
    fn write_writes_the_exact_bytes_and_a_second_write_changes_nothing() {
        let home = tempfile::tempdir().unwrap();
        let path = session_script_path(home.path());
        assert_eq!(path, home.path().join(".config/zellij/agent-session.sh"));
        assert_eq!(session_script_state(&path), ScriptState::Missing);
        // the directory does not exist yet
        assert_eq!(write_session_script(&path).unwrap(), ScriptState::Missing);
        assert_eq!(std::fs::read(&path).unwrap(), SESSION_SCRIPT.as_bytes());
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(write_session_script(&path).unwrap(), ScriptState::Current);
        assert_eq!(std::fs::read(&path).unwrap(), SESSION_SCRIPT.as_bytes());
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            modified
        );
        // no temporary file is left beside it
        let entries: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name())
            .collect();
        assert_eq!(entries, ["agent-session.sh"]);
    }

    #[test]
    fn an_older_script_and_an_edited_one_are_told_apart_and_both_rewritten() {
        let home = tempfile::tempdir().unwrap();
        let path = session_script_path(home.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let older = "#!/bin/sh\n# an older build's script\n";
        let older_hash = sha256_hex(older.as_bytes());
        std::fs::write(&path, older).unwrap();
        assert_eq!(
            script_state_against(&path, &[older_hash.as_str()]),
            ScriptState::Earlier
        );
        assert_eq!(script_state_against(&path, &[]), ScriptState::Edited);
        assert_eq!(write_session_script(&path).unwrap(), ScriptState::Edited);
        assert_eq!(std::fs::read(&path).unwrap(), SESSION_SCRIPT.as_bytes());
        std::fs::write(&path, format!("{}# a local tweak\n", SESSION_SCRIPT)).unwrap();
        assert_eq!(session_script_state(&path), ScriptState::Edited);
        assert_eq!(write_session_script(&path).unwrap(), ScriptState::Edited);
        assert_eq!(session_script_state(&path), ScriptState::Current);
    }

    /// `--write` modifies `--agent-hooks` and means nothing on its own.
    #[test]
    fn write_is_a_flag_of_agent_hooks() {
        use crate::cli::{CliArgs, Command};
        use clap::Parser;
        // Parsing builds the whole clap tree, which overflows a debug test thread's stack.
        crate::cli::on_big_stack(|| {
            let parsed = CliArgs::try_parse_from(["zellij", "setup", "--agent-hooks", "--write"]);
            match parsed.unwrap().command {
                Some(Command::Setup(setup)) => assert!(setup.agent_hooks && setup.write),
                other => panic!("expected setup, got {:?}", other),
            }
            assert!(CliArgs::try_parse_from(["zellij", "setup", "--write"]).is_err());
        });
    }

    #[cfg(unix)]
    #[test]
    fn write_goes_through_a_symlink() {
        let home = tempfile::tempdir().unwrap();
        let real = home.path().join("dotfiles-agent-session.sh");
        std::fs::write(&real, "#!/bin/sh\nold\n").unwrap();
        let path = session_script_path(home.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&real, &path).unwrap();
        assert_eq!(write_session_script(&path).unwrap(), ScriptState::Edited);
        assert!(path.is_symlink());
        assert_eq!(std::fs::read(&real).unwrap(), SESSION_SCRIPT.as_bytes());
    }

    #[test]
    fn pi_and_opencode_export_the_shared_pair() {
        assert!(hook("pi")
            .snippet
            .contains("process.env.AGENT_SESSION_ID = id"));
        assert!(hook("pi")
            .snippet
            .contains("process.env.AGENT_KIND = \"pi\""));
        let server = opencode_server();
        assert!(server.snippet.contains("event.env.AGENT_SESSION_ID = id"));
        assert!(server
            .snippet
            .contains("event.env.AGENT_KIND = \"opencode\""));
        assert!(server
            .snippet
            .contains("output.env.AGENT_SESSION_ID = input.sessionID"));
        assert!(server
            .snippet
            .contains("output.env.AGENT_KIND = \"opencode\""));
    }

    fn opencode_server() -> &'static AgentHook {
        AGENT_HOOKS
            .iter()
            .find(|hook| hook.kind == "opencode" && hook.file.contains("agent-session-env.ts"))
            .unwrap()
    }

    /// One file per surface serves opencode v1 and v2. Each version validates only its own key on
    /// the default export (v2: `id` and `setup`; v1: `server` or `tui`), so both keys sit on one
    /// object, and nothing is imported from either version's plugin package.
    #[test]
    fn the_opencode_hooks_serve_v1_and_v2_from_one_file() {
        for (hook, v1_key) in [
            (opencode_server(), "async server()"),
            (hook("opencode"), "async tui(api"),
        ] {
            let snippet = hook.snippet;
            assert!(snippet.contains("export default {"), "{}", hook.file);
            assert!(snippet.contains("  id: \""), "{}", hook.file);
            assert!(
                snippet.contains("  async setup(ctx: any)")
                    || snippet.contains("  setup(context: any)"),
                "{} lacks the v2 setup",
                hook.file
            );
            assert!(snippet.contains(v1_key), "{} lacks {}", hook.file, v1_key);
            assert!(
                !snippet.contains("@opencode"),
                "{} imports opencode",
                hook.file
            );
            assert!(hook.file.contains("v1 and v2"), "{}", hook.file);
        }
        // v1 shell.env is told the session; v2 needs the tool hook to pair it with the command
        assert!(opencode_server().snippet.contains("\"shell.env\""));
        // v2 finds the TUI package in plugins/ itself; v1 needs it named in tui.json
        let tui = hook("opencode");
        assert!(tui.file.contains("plugins/agent-session/"));
        assert!(tui.how.contains("\"./plugins/agent-session\""));
        assert!(tui.how.contains("tui.json"));
        assert!(!tui.how.contains("cli.json"));
        assert!(tui.snippet.contains("api.lifecycle.onDispose(() => {"));
    }

    /// opencode does not speak OSC 7501 itself, so its TUI plugin reports the state for it, from
    /// each version's own events: v2 hands an event's body over as `data`, v1 as `properties`.
    #[test]
    fn the_opencode_tui_reports_its_state_from_both_versions_events() {
        let tui = hook("opencode").snippet;
        for (version, needed) in [
            (
                "v2",
                &[
                    "data.on(type, (event: any) => handler(event.data))",
                    "on(\"session.execution.started\"",
                    "on(\"session.execution.succeeded\"",
                    "on(\"session.execution.interrupted\"",
                    "on(\"session.execution.failed\"",
                    "on(\"permission.asked\"",
                    "on(\"permission.replied\"",
                    "on(\"form.created\"",
                    "on(\"form.replied\"",
                    "on(\"form.cancelled\"",
                    "data.session.root(id)",
                    "data.session.status(id) === \"running\"",
                ][..],
            ),
            (
                "v1",
                &[
                    "api.event.on(type, (event: any) => handler(event.properties))",
                    "on(\"session.status\"",
                    "on(\"session.error\"",
                    "on(\"question.asked\"",
                    "on(\"question.replied\"",
                    "on(\"question.rejected\"",
                    "api.state.session.get(id)?.parentID",
                    "api.state.session.status(id)?.type",
                    "\"MessageAbortedError\"",
                ][..],
            ),
        ] {
            for needed in needed {
                assert!(tui.contains(needed), "{} lacks {}", version, needed);
            }
        }
        // the report: zellij-only, app=opencode, and only the states and kinds zellij keeps
        let write = tui.find(r"\x1b]7501;${next}:app=opencode\x07").unwrap();
        let guard = tui[..write]
            .rfind("if (!process.env.ZELLIJ) return")
            .unwrap();
        assert!(guard < write, "the state report is zellij-only");
        for state in [
            "state=blocked:kind=${ask.kind}",
            "\"state=working\"",
            "`state=error${text(t.failed)}`",
            "\"state=done\"",
            "\"state=clear\"",
        ] {
            assert!(tui.contains(state), "lacks {}", state);
        }
        for kind in ["\"permission\"", "\"question\""] {
            assert!(tui.contains(kind), "lacks blocked kind {}", kind);
        }
        // zellij drops the whole report for a control character in its text
        assert!(tui.contains(r"replace(/[\u0000-\u001f\u007f-\u009f]+/g"));
        // a report is sent only when it changes
        assert!(tui.contains("if (!next || next === sent) return"));
        // a new session runs before the TUI shows it, so every top session is tracked, and the
        // one shown is reported
        assert!(tui.contains("const roots = new Map<string, Tracked>()"));
        assert!(tui.contains("if (root === shown) report()"));
    }

    #[test]
    fn the_codex_hook_starts_and_ends_and_says_it_cannot_export() {
        let codex = hook("codex");
        assert!(codex.snippet.contains("agent-session.sh codex start"));
        assert!(codex.snippet.contains("[[hooks.SessionEnd]]"));
        assert!(codex.snippet.contains("agent-session.sh codex end"));
        assert!(codex.covers.contains("No env export"));
    }

    /// codex does not speak OSC 7501 itself, so its hooks report the state for it.
    #[test]
    fn the_codex_hook_reports_its_state() {
        let codex = hook("codex");
        for needed in [
            "[[hooks.UserPromptSubmit]]",
            "agent-session.sh codex state working",
            "[[hooks.PermissionRequest]]",
            "agent-session.sh codex state blocked permission",
            "[[hooks.Stop]]",
            "agent-session.sh codex state done",
        ] {
            assert!(codex.snippet.contains(needed), "codex lacks {}", needed);
        }
        let guard = SESSION_SCRIPT
            .find(r#"[ -n "$ZELLIJ" ] || exit 0"#)
            .unwrap();
        let report = SESSION_SCRIPT.find(r"\033]7501;").unwrap();
        assert!(guard < report, "the state report is zellij-only");
        // the state mode must not wait for a session id it is never given
        let state_mode = SESSION_SCRIPT.find(r#"if [ "$mode" = state ]"#).unwrap();
        let id_required = SESSION_SCRIPT.find(r#"[ -n "$id" ] || exit 0"#).unwrap();
        assert!(state_mode < id_required);
    }

    #[test]
    fn the_output_names_each_agent_its_file_and_what_it_covers() {
        let text = agent_hooks_text();
        assert!(text.contains(&format!("# file: {}", SESSION_SCRIPT_PATH)));
        for hook in AGENT_HOOKS {
            assert!(text.contains(&format!("## {}\n# file: {}\n", hook.kind, hook.file)));
            assert!(text.contains(&format!("# covers: {}\n", hook.covers)));
            assert!(text.contains(hook.snippet));
        }
    }
}
