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
//! Print only. Nothing here writes a file: the operator installs the snippets, usually through
//! dotfiles. Pure strings, so it builds for wasm with the rest of the ungated half.

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

/// Both harnesses hand their hook a JSON object on stdin with a `session_id` field. A hook's
/// stdout is the agent's, not the terminal, and a claude hook has no controlling terminal at all -
/// verified with Claude Code 2.1: `ps -o tty=` prints `?` for it - so the OSC step walks up its
/// ancestors to the agent itself, by executable name or argv0, and writes to the agent's terminal
/// device. It stops at the agent: a `codex exec` with no terminal of its own climbed past it, in a
/// test, to the terminal of an unrelated pane further up.
pub const SESSION_SCRIPT: &str = r#"#!/bin/sh
# Agent session hook (zellij setup --agent-hooks). Usage: sh agent-session.sh KIND start|end|osc
#   start: register the session in the agent-sessions registry, then report it to zellij
#   end:   unregister it
#   osc:   only report it to zellij
# Reads the hook's JSON on stdin for its "session_id".
kind=$1
mode=${2:-start}
id=$(tr -d '\n' | sed -n 's/.*"session_id"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')
[ -n "$id" ] || exit 0
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
        covers:
            "the zellij report only. The env export ($CLAUDE_ENV_FILE), register and the \
                 SessionEnd unregister live in bm's SessionStart hook for now; do not add them here",
        snippet: r#"{
  "hooks": {
    "SessionStart": [
      {
        "hooks": [
          { "type": "command", "command": "sh ~/.config/zellij/agent-session.sh claude osc" }
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
        covers: "register on SessionStart, unregister on SessionEnd, and the zellij report. No \
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
        covers: "register when the TUI opens a session, unregister when the TUI exits, and the \
                 zellij report. The env export is the server plugin above; a one-shot \
                 `opencode run` has no TUI and is not registered",
        snippet: r#"import { execFile, execFileSync } from "node:child_process"
import { writeFileSync } from "node:fs"

// Agent session hook (zellij setup --agent-hooks). Runs in the TUI, the process in the pane. One
// file serves opencode v1 and v2: v2 calls `setup`, v1 calls `tui`. Nothing is imported from
// opencode, because neither version can resolve the other's plugin package.
const REGISTRY = 'command -v agent-sessions >/dev/null && exec agent-sessions "$@"'

// Register and report the session the TUI shows, each time it changes. Returns the cleanup.
function watch(sessionOf: () => string) {
  let current = ""
  const check = () => {
    const id = sessionOf()
    if (!id || id === current) return
    current = id
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

export default {
  id: "agent-session",
  // v2: the route is a function on context.ui.router
  setup(context: any) {
    return watch(() => {
      const route = context.ui.router.current()
      return route?.type === "session" ? route.sessionID : ""
    })
  },
  // v1: the route is a property on api.route
  async tui(api: any) {
    const stop = watch(() => {
      const route = api.route.current
      return route?.name === "session" ? route.params?.sessionID ?? "" : ""
    })
    api.lifecycle.onDispose(stop)
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
         # Nothing here is written for you.\n",
    );
    text.push_str(&format!(
        "\n## shared hook script (claude, codex)\n# file: {}\n# create it; it needs no execute bit\n\n{}",
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

    /// claude's export, register and unregister belong to bm's hook today; this one only reports.
    #[test]
    fn the_claude_hook_only_reports_and_says_where_the_rest_lives() {
        let claude = hook("claude");
        assert!(claude.snippet.contains("agent-session.sh claude osc"));
        assert!(!claude.snippet.contains("agent-sessions"));
        assert!(claude.covers.contains("bm's SessionStart hook"));
        let value: serde_json::Value = serde_json::from_str(claude.snippet).unwrap();
        assert!(value["hooks"]["SessionStart"][0]["hooks"][0]["command"].is_string());
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
        assert!(tui.snippet.contains("api.lifecycle.onDispose(stop)"));
    }

    #[test]
    fn the_codex_hook_starts_and_ends_and_says_it_cannot_export() {
        let codex = hook("codex");
        assert!(codex.snippet.contains("agent-session.sh codex start"));
        assert!(codex.snippet.contains("[[hooks.SessionEnd]]"));
        assert!(codex.snippet.contains("agent-session.sh codex end"));
        assert!(codex.covers.contains("No env export"));
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
