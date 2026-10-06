//! `zellij setup --agent-hooks`: the snippet each coding agent needs to tell zellij which session
//! its pane holds.
//!
//! The report is `OSC 1337 SetUserVar=agent_session=<base64 id>`, the iTerm2 and WezTerm user
//! variable convention, printed to the pane's own terminal. The server keeps it on that pane and
//! prefers it over every other source when it records the pane's resume command. See FORK.md,
//! "A restored session offers to resume its agents, and only its agents".
//!
//! Print only. Nothing here writes a file: the operator installs the snippets, usually through
//! dotfiles. Every snippet is a no-op outside zellij - it returns at once unless `ZELLIJ` is set -
//! so a config shared with machines and terminals that do not run zellij costs nothing there.
//!
//! Pure strings, so it builds for wasm with the rest of the ungated half.

/// One agent's install instructions.
pub struct AgentHook {
    /// The harness, as `agent_detect` names it.
    pub kind: &'static str,
    /// The file the snippet goes in.
    pub file: &'static str,
    /// What to do with the snippet, in one line.
    pub how: &'static str,
    /// The snippet itself, exactly as it goes in the file.
    pub snippet: &'static str,
    /// What is not proven about it, if anything.
    pub caveat: Option<&'static str>,
}

/// The shared hook script `claude` and `codex` call. Both hand their hook a JSON object on stdin
/// with a `session_id` field. A hook's stdout is the agent's, not the terminal, and a claude hook
/// has no controlling terminal at all - verified with Claude Code 2.1: `ps -o tty=` prints `?` for
/// it - so the script walks up its ancestors to the first one with a terminal, the agent, and
/// writes the report to that terminal device, which is the pane's.
pub const SESSION_SCRIPT_PATH: &str = "~/.config/zellij/agent-session.sh";

pub const SESSION_SCRIPT: &str = r#"#!/bin/sh
# Tell zellij which session this pane's agent holds, so a restored pane resumes exactly that one.
# Reads the hook's JSON on stdin and prints OSC 1337 SetUserVar=agent_session to the pane.
[ -n "$ZELLIJ" ] || exit 0
id=$(tr -d '\n' | sed -n 's/.*"session_id"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')
[ -n "$id" ] || exit 0
# A hook runs with no controlling terminal: claude starts it in a session of its own, so
# /dev/tty fails. The agent that started it holds the pane's terminal; walk up to it.
p=$$
while [ "${p:-1}" -gt 1 ]; do
  t=$(ps -o tty= -p "$p" 2>/dev/null | tr -d ' ')
  case "$t" in
    ''|'?'|'??') p=$(ps -o ppid= -p "$p" 2>/dev/null | tr -d ' ') ;;
    *) break ;;
  esac
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
        how: "merge into the top-level \"hooks\" object; it fires on start-up, /clear, /resume and compaction",
        snippet: r#"{
  "hooks": {
    "SessionStart": [
      {
        "hooks": [
          { "type": "command", "command": "sh ~/.config/zellij/agent-session.sh" }
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
        snippet: r#"[[hooks.SessionStart]]

[[hooks.SessionStart.hooks]]
type = "command"
command = "sh ~/.config/zellij/agent-session.sh"
timeout = 5"#,
        caveat: Some(
            "not proven in an interactive codex: if codex runs the hook in its app-server daemon, \
             the hook has no pane terminal and reports nothing; the open rollout file still answers",
        ),
    },
    AgentHook {
        kind: "opencode",
        file: "~/.config/opencode/plugins/zellij-agent-session.js",
        how: "create the file; opencode loads every file in that directory at start-up",
        snippet: r#"import { writeFileSync } from "node:fs"

// Tell zellij which session this pane's opencode holds.
export const ZellijAgentSession = async () => {
  let last = ""
  return {
    event: async ({ event }) => {
      if (!process.env.ZELLIJ) return
      const info = event.properties?.info
      if (event.type !== "session.updated" || !info?.id || info.parentID) return
      if (info.id === last) return
      last = info.id
      const b64 = Buffer.from(info.id).toString("base64")
      try {
        writeFileSync("/dev/tty", `\x1b]1337;SetUserVar=agent_session=${b64}\x07`)
      } catch {}
    },
  }
}"#,
        caveat: Some("not proven: opencode was not installed where this was written"),
    },
    AgentHook {
        kind: "pi",
        file: "~/.pi/agent/extensions/zellij-agent-session.ts",
        how: "create the file; pi loads every extension in that directory at start-up",
        snippet: r#"import { writeFileSync } from "node:fs";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";

// Tell zellij which session this pane's pi holds: at start-up, /new, /resume and /fork.
export default function zellijAgentSession(pi: ExtensionAPI) {
  pi.on("session_start", async (_event, ctx) => {
    if (!process.env.ZELLIJ) return;
    const id = ctx.sessionManager.getSessionId();
    if (!id) return;
    const b64 = Buffer.from(id).toString("base64");
    try {
      writeFileSync("/dev/tty", `\x1b]1337;SetUserVar=agent_session=${b64}\x07`);
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
        "# Agent session hooks for zellij\n\
         #\n\
         # Each agent reports the session its pane holds with OSC 1337 \
         SetUserVar=agent_session=<base64 id>.\n\
         # zellij keeps it on that pane and records the pane's resume command with that exact id.\n\
         # Every snippet does nothing unless ZELLIJ is set. Nothing here is written for you.\n",
    );
    text.push_str(&format!(
        "\n## shared hook script (claude, codex)\n# file: {}\n# create it; it needs no execute bit\n\n{}",
        SESSION_SCRIPT_PATH, SESSION_SCRIPT
    ));
    for hook in AGENT_HOOKS {
        text.push_str(&format!(
            "\n## {}\n# file: {}\n# {}\n",
            hook.kind, hook.file, hook.how
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

    #[test]
    fn every_hook_is_a_no_op_outside_zellij_and_prints_the_user_var() {
        assert!(SESSION_SCRIPT.contains(r#"[ -n "$ZELLIJ" ] || exit 0"#));
        assert!(SESSION_SCRIPT.contains("SetUserVar=agent_session="));
        assert!(SESSION_SCRIPT.contains(r#"> "/dev/$t""#));
        for hook in AGENT_HOOKS {
            let script_based = hook.snippet.contains(SESSION_SCRIPT_PATH);
            assert!(
                script_based
                    || (hook.snippet.contains("process.env.ZELLIJ")
                        && hook.snippet.contains("SetUserVar=agent_session=")),
                "{} does not guard on ZELLIJ or does not print the user var",
                hook.kind
            );
        }
    }

    #[test]
    fn the_output_names_each_agent_and_its_file() {
        let text = agent_hooks_text();
        assert!(text.contains(&format!("# file: {}", SESSION_SCRIPT_PATH)));
        for hook in AGENT_HOOKS {
            assert!(text.contains(&format!("## {}\n# file: {}\n", hook.kind, hook.file)));
            assert!(text.contains(hook.snippet));
        }
    }

    #[test]
    fn the_claude_snippet_is_valid_json() {
        let claude = AGENT_HOOKS
            .iter()
            .find(|hook| hook.kind == "claude")
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(claude.snippet).unwrap();
        assert!(value["hooks"]["SessionStart"][0]["hooks"][0]["command"].is_string());
    }
}
