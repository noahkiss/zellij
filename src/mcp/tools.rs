//! The seven tools, and the half of each one that is generated rather than written.
//!
//! A tool's description is routing logic: it decides whether the tool is called at all, so the
//! part that says what the tool is FOR is written by hand and kept blunt. The part that says what
//! comes back is not written here at all - it is generated from the same `OUTPUTS` table
//! `zellij setup --dump-surface` reads, and so is every input-schema property that stands for a
//! real CLI flag. A flag renamed in `cli.rs` therefore cannot leave a stale description behind:
//! `every_parameter_names_a_real_argument` fails the build instead.
//!
//! Seven, not one per verb. `zellij action` has eighty-seven of those, and a surface nobody can
//! route through is worse than a small one that names its own follow-ups.

use std::borrow::Cow;
use std::sync::Arc;

use rmcp::model::{JsonObject, Tool, ToolAnnotations};
use serde_json::{json, Map, Value};
use zellij_utils::cli_surface;

use super::invoke;

/// What a tool parameter is, in JSON Schema terms.
pub enum ParamKind {
    Str,
    Bool,
    Int,
    /// A closed set, written out here because the tool's own values need not be the CLI's.
    Enum(&'static [&'static str]),
}

/// One parameter of one tool.
pub struct ParamSpec {
    /// The property name the caller passes. Unambiguous on its own - `pane`, not `id`.
    pub name: &'static str,
    pub kind: ParamKind,
    pub required: bool,
    /// The CLI command and argument this property stands for, as `--dump-surface` names them.
    /// A renamed flag then fails the build rather than leaving a stale property behind. `None` for
    /// a property the tool invents.
    pub from: Option<(&'static str, &'static str)>,
    /// What this property is, in the caller's terms.
    ///
    /// Clap's help is the fallback rather than the rule: it is written for a command line, and a
    /// line that says "after a `--`", "one per argument", "pass `-` for stdin" or "without this,
    /// the focused pane" describes an argv this caller never writes. So a non-empty `describe`
    /// wins over the inherited help, and `from` goes on standing guard over the flag's name.
    pub describe: &'static str,
    pub default: Option<&'static str>,
}

/// One tool: what is written by hand, and where the rest is generated from.
pub struct ToolSpec {
    pub name: &'static str,
    /// The first line. Imperative, blunt, no hedging.
    pub summary: &'static str,
    /// The situation this tool is the right answer to.
    pub best_for: &'static str,
    /// When NOT to use it. Ambiguity about scope is the main cause of a wrong tool being picked.
    pub not_for: &'static str,
    /// The tool to reach for next, and when.
    pub follow_up: Option<&'static str>,
    /// The CLI commands whose printed output this tool returns. The `Returns:` line is generated
    /// from their rows in the surface map, so it cannot promise a key nothing prints.
    ///
    /// A list, because a tool that multiplexes several operations returns a different shape for
    /// each of them, and one command's row cannot speak for the others. One entry is the ordinary
    /// case; several make the line say which operation returns what.
    pub reports: &'static [&'static str],
    /// Anything the caller has to know that the generated half cannot say - a discipline the CLI
    /// enforces, a value that has to be named explicitly. Empty for a tool with none.
    pub tips: &'static str,
    pub params: &'static [ParamSpec],
    pub read_only: bool,
    /// Whether a call can destroy something a caller cannot get back. Only meaningful when
    /// `read_only` is false.
    pub destructive: bool,
    /// Whether repeating a call leaves the session as one call would. A read-only tool changes
    /// nothing, so it is always true for one of those - the hint only carries information for a
    /// tool that writes.
    pub idempotent: bool,
}

/// The `session` parameter, which every tool takes and none of them require.
const SESSION: ParamSpec = ParamSpec {
    name: "session",
    kind: ParamKind::Str,
    required: false,
    from: None,
    describe: "The session to act on. Defaults to ZELLIJ_SESSION_NAME in this server's own \
               environment, which is the session it was started from. host/session acts on a \
               session on another machine over ssh - boxa/work - and host/ on whichever session \
               that machine's CLI picks. The host must be listed in remote_hosts in config.kdl.",
    default: None,
};

pub const TOOLS: &[ToolSpec] = &[
    ToolSpec {
        name: "zellij_overview",
        summary: "List what a zellij session contains: its panes, the agents running in them, or \
                  the sessions on this machine - or every agent on every listed host.",
        best_for: "the first call of any zellij task - it is how you learn the pane handles \
                   everything else is addressed by, and which panes are running a coding agent.",
        not_for: "reading what is ON a pane's screen, and not for watching a pane over time.",
        follow_up: Some(
            "zellij_read_pane to read one pane's screen, or zellij_wait_for to block until \
             something happens in it",
        ),
        // one row per scope: `panes` walks list-panes, `agents` walks list-agents and `sessions`
        // asks the machine with `ls`. One entry promised the pane columns for all three
        reports: &["action list-panes", "action list-agents", "list-sessions"],
        tips: "Call this before zellij_create: a pane you already own is cheaper than a new one, \
               and its handle is in this answer. scope=agents narrows the same walk to the panes \
               running claude, opencode, codex or pi, each with a live name (Claude only for now), \
               a self marker and the harness's own session id where \
               it exports one, the state it reports (state, state_kind, state_msg) and how many \
               seconds ago it last reported (state_age_s) and last drew anything (output_age_s): \
               the supervision view. scope=my_agents is the same, for only the agents in your own \
               tab, which is where every agent you started with zellij_create went. A working \
               agent whose output_age_s keeps growing has stalled. scope=sessions answers about \
               the machine rather than about one \
               session. scope=fleet lists every agent in every session on this machine and on \
               every host in remote_hosts, each row with its host and session; a host that does \
               not answer within 10 seconds is one `unreachable` row, and the rest still answer. \
               To act on a fleet row, pass session as host/session. scope=agents, my_agents and \
               fleet also answer grouped by host, tab and pane: each pane carries its address \
               and, when a harness can reach it, message_to; how_to_message says how to use them.",
        params: &[
            SESSION,
            ParamSpec {
                name: "scope",
                kind: ParamKind::Enum(&["panes", "agents", "my_agents", "sessions", "fleet"]),
                required: false,
                from: None,
                describe: "What to list: every pane of the session, only the panes running a \
                           coding agent, only the agents in your own tab, the sessions on \
                           this machine, or every agent on this machine and every remote host.",
                default: Some("panes"),
            },
            ParamSpec {
                name: "include_hidden",
                kind: ParamKind::Bool,
                required: false,
                from: Some(("action list-panes", "--all")),
                describe: "",
                default: None,
            },
        ],
        read_only: true,
        destructive: false,
        idempotent: true,
    },
    ToolSpec {
        name: "zellij_read_pane",
        summary: "Read what is on a pane's screen right now, or the last reply of the coding \
                  agent in it.",
        best_for: "seeing the output of something a pane is running, including a pane in a tab \
                   nobody is looking at - the grid is kept whether or not the pane renders. With \
                   last_reply, checking what an agent in another pane answered.",
        not_for: "waiting for output that has not arrived yet, and not for a pane you cannot \
                  name: there is no default pane here.",
        follow_up: Some("zellij_wait_for when the output you want is not there yet"),
        reports: &["action dump-screen", "action last-reply"],
        tips: "This is the grid, not the frame. A pane held open after its command exited shows \
               `[ EXIT CODE: n ]` in its border, and that line is NOT in this answer. Use \
               zellij_wait_for with until=exit for a command's status. last_reply reads the \
               agent's own transcript instead of the screen: the text it wrote after its last \
               prompt or tool call, whole, however long, without the narration between tool \
               calls. It reads claude, codex and pi, from the transcript on this machine; for \
               opencode, or an agent that has not been sent a message yet, the call misses and \
               says why - read the screen then. Wait with until=agent_idle first, so the reply \
               is the finished one.",
        params: &[
            SESSION,
            ParamSpec {
                name: "pane",
                kind: ParamKind::Str,
                required: true,
                from: Some(("action dump-screen", "--pane-id")),
                describe: "The pane to read: its handle, like sunny-otter, or terminal_1, \
                           plugin_2, a bare integer or a pane uuid. zellij_overview prints every \
                           one of them.",
                default: None,
            },
            ParamSpec {
                name: "full",
                kind: ParamKind::Bool,
                required: false,
                from: Some(("action dump-screen", "--full")),
                describe: "",
                default: None,
            },
            ParamSpec {
                name: "ansi",
                kind: ParamKind::Bool,
                required: false,
                from: Some(("action dump-screen", "--ansi")),
                describe: "",
                default: None,
            },
            ParamSpec {
                name: "last_reply",
                kind: ParamKind::Bool,
                required: false,
                from: None,
                describe: "Return only the last reply of the coding agent in the pane, read from \
                           its transcript, instead of the screen. Not with full or ansi.",
                default: None,
            },
        ],
        read_only: true,
        destructive: false,
        idempotent: true,
    },
    ToolSpec {
        name: "zellij_wait_for",
        summary: "Block until a pane's command exits, its output matches a pattern, it falls \
                  silent, its shell is ready for the next command, or the coding agent in it \
                  stops working.",
        best_for: "the step after starting something long: wait for it rather than reading the \
                   pane over and over.",
        not_for: "a pane that is already in the state you want - read it instead. This call \
                  blocks, so it is not free.",
        follow_up: Some("zellij_read_pane to read what the pane says once the wait returns"),
        reports: &["action wait"],
        tips:
            "The exit_code of THIS call says whether the wait succeeded, not whether the command \
               did. A command that failed still returns exit_code 0 here: read `exit_status` in \
               the result. A wait that times out is a miss, not an error, and says so. until=match \
               needs a pattern, a regex tested line by line (^ and $ anchor to the line); until=quiet \
               takes the window in quiet_ms. until=match sees only lines that arrive AFTER the \
               wait began: text already on screen never matches, and neither does a line identical \
               to one already on screen, so make the output you wait for unique. Match a short \
               string rather than a line that may wrap. until=exit on a plain shell pane waits for the shell, which is a \
               timeout. until=ready means the command you typed has finished: the shell drew a \
               prompt AFTER the last input typed into the pane, so the prompt still on screen \
               right after typing does not count. Use it after zellij_write_input sends a line \
               to a shell; read `ready_by` (prompt, or quiet for a shell without OSC 133 marks, \
               where quiet_ms is the window). Every wait is bounded: without timeout_s it gives \
               up after {wait_timeout_s} seconds rather than blocking for the life of the pane. If your MCP \
               client cuts a request off sooner (many do at 60 seconds), pass a timeout_s under \
               that deadline and wait again. \
               Start the child, then wait for agent-change in the background; never poll the screen. \
               until=agent_change wakes on agent state, identity or name changes. Omit pane to watch \
               every agent except the caller. until=agent_idle is for a pane running a coding agent: it returns when the agent \
               stops working - its turn ended (`state` idle, done or error) or it is blocked on a \
               person (`state` blocked, with `state_kind` permission, question or auth and the \
               question in `state_msg`). Read `state` to tell which. until=agent_waiting returns \
               only for blocked, and misses when the agent ends its turn without asking. Both \
               count only a state the agent reported after the last input typed into the pane, \
               so wait straight after zellij_write_input sends a message, without a pause. An \
               agent that reports no state (opencode, or codex without its hooks) is judged by \
               3 seconds of silence for agent_idle and says `idle_by: quiet`; agent_waiting \
               cannot tell there and misses.",
        params: &[
            SESSION,
            ParamSpec {
                name: "pane",
                kind: ParamKind::Str,
                required: false,
                from: Some(("action wait", "pane_id")),
                describe: "Pane target. Optional only for until=agent_change; omitting it watches other agents in the session.",
                default: None,
            },
            ParamSpec {
                name: "until",
                kind: ParamKind::Enum(&[
                    "exit",
                    "match",
                    "quiet",
                    "ready",
                    "agent_idle",
                    "agent_waiting",
                    "agent_change",
                ]),
                required: false,
                from: Some(("action wait", "--for")),
                describe: "",
                default: Some("exit"),
            },
            ParamSpec {
                name: "pattern",
                kind: ParamKind::Str,
                required: false,
                from: Some(("action wait", "--match")),
                describe: "The regex a new line must match, when until=match. Rust regex syntax, \
                           unanchored, tested one screen line at a time. Text already on screen \
                           when the wait begins does not count.",
                default: None,
            },
            ParamSpec {
                name: "quiet_ms",
                kind: ParamKind::Int,
                required: false,
                from: Some(("action wait", "--quiet-ms")),
                describe: "How long a pane must produce nothing to count as quiet, when \
                           until=quiet, or ready for a shell without OSC 133 marks. For \
                           until=agent_idle on an agent that reports no state, 3000 unless \
                           given.",
                default: Some("500"),
            },
            ParamSpec {
                name: "timeout_s",
                kind: ParamKind::Int,
                required: false,
                from: Some(("action wait", "--timeout")),
                describe: "Seconds to wait before the wait gives up and answers with a miss. The \
                           answer comes a fraction of a second after this, so keep it at least 5 \
                           seconds under your MCP client's own request deadline, which is often \
                           60 seconds: a client that gives up first returns a request error \
                           instead of this tool's answer. To wait longer, wait again.",
                default: Some(WAIT_TIMEOUT_PLACEHOLDER),
            },
        ],
        read_only: true,
        // `idempotentHint` describes what repeating a call does to the session, and a read-only
        // call does nothing to it. Saying `false` here while the other read-only tool says `true`
        // was noise a client could route on
        destructive: false,
        idempotent: true,
    },
    ToolSpec {
        name: "zellij_write_input",
        summary: "Type into a named pane, as if at its keyboard, or send a coding agent in it a \
                  message.",
        best_for: "driving a program that is already running in a pane - answering a prompt, \
                   sending a line to a shell, talking to an agent in another pane.",
        not_for: "starting something in a NEW pane, and not for a pane you have not named. There \
                  is no focused pane here: an unnamed target would be a pane you have never seen.",
        follow_up: Some("zellij_wait_for or zellij_read_pane to see what the pane did with it"),
        // `keys` presses keys and `text` writes characters, which are two verbs, not one with a
        // flag: a tool that multiplexes says what each of its commands returns
        reports: &["action send-keys", "action write-chars"],
        tips: "keys goes through the key parser, so `Enter`, `Ctrl c` and `Esc` mean those keys; \
               text is written literally and presses nothing. Pass one of keys or text. This \
               is the cheap way to do more work in a pane you already made: reach for it before \
               zellij_create. To message a coding agent, use zellij_send: it waits until the \
               agent is idle and nobody is typing there. Successful input to an agent adds that \
               hint. To send pane input, pass text with \
               submit=true: the text goes in as one bracketed paste, so its newlines do not send \
               it early, and then Enter sends it. Follow with zellij_wait_for until=agent_idle.",
        params: &[
            SESSION,
            ParamSpec {
                name: "pane",
                kind: ParamKind::Str,
                required: true,
                from: Some(("action send-keys", "--pane-id")),
                describe: "The pane to type into: its handle, like sunny-otter, or terminal_1, \
                           plugin_2, a bare integer or a pane uuid. Required - there is no focused \
                           pane here.",
                default: None,
            },
            ParamSpec {
                name: "keys",
                kind: ParamKind::Str,
                required: false,
                from: Some(("action send-keys", "keys")),
                describe: "One key to press, after any modifiers, space separated: `Enter`, \
                           `Ctrl c`, `Alt Shift x`, `F1`. Modifiers are Ctrl, Alt, Shift and Super; \
                           `C-c` and `Ctrl-c` are not keys. One key per call.",
                default: None,
            },
            ParamSpec {
                name: "text",
                kind: ParamKind::Str,
                required: false,
                from: Some(("action write-chars", "chars")),
                describe:
                    "The text to write literally. It presses nothing: add a second call with \
                           keys `Enter` to submit it, or pass submit.",
                default: None,
            },
            ParamSpec {
                name: "submit",
                kind: ParamKind::Bool,
                required: false,
                from: None,
                describe: "With text: paste it as one bracketed paste, then press Enter. How to \
                           send a message to a coding agent, or several lines to a shell at once.",
                default: None,
            },
        ],
        read_only: false,
        destructive: false,
        idempotent: false,
    },
    ToolSpec {
        name: "zellij_send",
        summary: "Send a message to a coding agent in another pane, on this machine or another, \
                  typed in only when that agent is idle and nobody is typing there.",
        best_for: "telling another agent something - a request, an answer, a hand-off - by host, \
                   tab, handle, agent name or project. `develop on boxa` is to=boxa/develop.",
        not_for: "a pane that runs no coding agent - type into that with zellij_write_input - and \
                  not for keys or a command line.",
        // no follow-up: the answer comes back typed into the sender's own prompt
        follow_up: None,
        reports: &["action send-message", "action message-cancel"],
        tips: "The receiving session types the message in as one paste and an Enter only when the \
               agent reported idle, done or error, no unsent text is in its prompt, and no key \
               reached the pane in the quiet window (5 seconds unless the receiving machine's \
               agent_message_quiet says otherwise). Otherwise it holds the message and tries \
               again each second: status queued, with reason working, blocked, draft, typing or \
               starting. A held message survives a restart of that session and expires after \
               expire_s, or when you give none, after the receiving machine's configured \
               default (agent_message_expire, 30 minutes unset); you then get a notice from \
               zellij, in another session or on another machine too. receipt=true asks for a \
               one-line notice from zellij when a held message is delivered; a message delivered \
               at once gets none, since this call already says delivered. A receipt costs you a \
               turn when it arrives, so ask only when the timing matters; `zellij action \
               message-status <id>` in that session polls instead. A message that lands late \
               tells the receiver how long ago it was queued. Refused means not sent: not_agent, no_state, or a target that named no \
               agent or several, with the candidates listed. The receiver reads, with your text, \
               an envelope that names you and the message id; it answers with reply_to=<id>. A \
               Claude Code receiver gets a cross-session message, shown collapsed when your text \
               is one short line. An OpenCode receiver whose server the session finds gets the \
               message through that server instead, whole and at once (via opencode-api), shown \
               as one line. A message whose paste passes 600 characters arrives as a \
               one-line stub, its first line and a note; the receiver reads the whole text with \
               `zellij action message-show <id>` in its shell. cancel=<id> takes back a message \
               still queued, with session=<host/session> from its record when that is not your \
               own session. A \
               machine whose zellij has no send-message is refused, not typed into. Do not wait \
               for the answer: it arrives typed into your own prompt as a new message once you \
               are idle, so end your turn or carry on.",
        params: &[
            SESSION,
            ParamSpec {
                name: "to",
                kind: ParamKind::Str,
                required: false,
                from: Some(("action send-message", "--to")),
                describe: "Who gets it: a pane handle, a tab name, an agent name or a project \
                           directory name, optionally after host/ and session/: boxa/develop, \
                           boxa/work/develop. With no session, every session on that machine \
                           is searched; exactly one agent must match. Required unless reply_to.",
                default: None,
            },
            ParamSpec {
                name: "text",
                kind: ParamKind::Str,
                required: false,
                from: Some(("action send-message", "text")),
                describe: "The message. Newlines are kept; it is sent as one paste. Required \
                           unless cancel.",
                default: None,
            },
            ParamSpec {
                name: "reply_to",
                kind: ParamKind::Str,
                required: false,
                from: Some(("action send-message", "--reply-to")),
                describe: "The id of a message you received, from its envelope. Without `to`, \
                           the reply goes to that message's sender, wherever it is.",
                default: None,
            },
            ParamSpec {
                name: "expire_s",
                kind: ParamKind::Int,
                required: false,
                from: Some(("action send-message", "--expire-s")),
                describe: "Seconds a held message waits before it expires. 0 means deliver now \
                           or not at all. Leave it out to use the receiving machine's configured \
                           default.",
                default: None,
            },
            ParamSpec {
                name: "wait_s",
                kind: ParamKind::Int,
                required: false,
                from: Some(("action send-message", "--wait")),
                describe: "Block until a held message is delivered, for at most this many \
                           seconds. It waits for delivery only, never for the answer. Keep it \
                           under your client's own request deadline.",
                default: None,
            },
            ParamSpec {
                name: "from_person",
                kind: ParamKind::Bool,
                required: false,
                from: Some(("action send-message", "--from-person")),
                describe: "Set it only when the text is the person's own words or request, which \
                           you relay for them: the receiver is told it comes directly from that \
                           person, by name. Never set it for your own messages.",
                default: None,
            },
            ParamSpec {
                name: "receipt",
                kind: ParamKind::Bool,
                required: false,
                from: Some(("action send-message", "--receipt")),
                describe: "Be told, by a one-line notice from zellij, when a message that was \
                           held is delivered. It costs you a turn; leave it out unless the \
                           timing matters. A message delivered at once gets no receipt.",
                default: None,
            },
            ParamSpec {
                name: "cancel",
                kind: ParamKind::Str,
                required: false,
                from: Some(("action message-cancel", "id")),
                describe: "Take back a message you sent that is still queued, by the id in its \
                           record; it is never typed in. Pass it alone, with `session` set to the \
                           record's host/session when the message is held in another session. A \
                           delivered or expired message cannot be cancelled.",
                default: None,
            },
        ],
        read_only: false,
        destructive: false,
        idempotent: false,
    },
    ToolSpec {
        name: "zellij_create",
        summary: "Make a pane to work in, optionally running a command in it.",
        best_for: "starting work when you have no pane of your own yet. One pane per job is the \
                   norm; after that, write to the one you have.",
        not_for:
            "running something in a pane that already exists - write to it instead. Not for a \
                  second pane when your first one is idle, and not for replacing a pane whose \
                  command failed: reuse it, or close it.",
        follow_up: Some("zellij_write_input or zellij_wait_for, using the handle this returns"),
        // `kind: tab` runs `new-tab`, which prints the same three keys today and need not keep
        // doing so. A tool that multiplexes says what each of its commands returns
        reports: &["action new-pane", "action new-tab", "action reopen-pane"],
        tips:
            "By default the pane runs your interactive shell, with your full rc files, and the \
               call waits until that shell is ready before it types `command` and presses Enter - \
               what you would get by typing it yourself. The pane stays a shell after the command \
               ends. Success means the pane was MADE and the command typed, not that it worked: \
               follow with zellij_wait_for until=match or until=quiet, or zellij_read_pane. For a \
               one-shot command whose exit status you want, pass run_in=exec and follow with \
               zellij_wait_for until=exit, then read `exit_status`. A shell that is not ready in \
               time is reported as `ready: false` with the pane's handle, and nothing is typed. \
               The answer carries the pane's id and its generated handle. The handle is the \
               pane's address and survives a session restore: use it, not the integer id, for \
               every later call on the pane. You cannot choose a pane's name or handle; a pane \
               running a command is named after the command. kind=tab takes `name` for the \
               tab itself. On another machine (session host/session) kind is \
               agent_tab or tab, cwd is an absolute path there, and exec runs through /bin/sh.",
        params: &[
            SESSION,
            ParamSpec {
                name: "kind",
                kind: ParamKind::Enum(&["agent_tab", "stacked", "tab"]),
                required: false,
                from: None,
                describe: "Where the pane goes. agent_tab, the default, is your own tab - the tab \
                           you are in with `-zj` on the end, made once, reused after, and it does \
                           not take the person's focus. It is named after the tab this server's \
                           own pane is in, even when `session` names a different session. Use it \
                           unless you have a reason not to. stacked stacks onto your own pane. \
                           tab makes a fresh tab, without going there. This tool never splits \
                           or floats.",
                default: Some(invoke::CREATE_KIND_DEFAULT),
            },
            ParamSpec {
                name: "command",
                kind: ParamKind::Str,
                required: false,
                from: Some(("action new-pane", "command")),
                describe: "The command to run in the pane, through your shell: quotes, pipes, \
                           `&&`, `~` and `$VAR` all work. Without one the pane runs your shell.",
                default: None,
            },
            ParamSpec {
                name: "run_in",
                kind: ParamKind::Enum(&["shell", "exec"]),
                required: false,
                from: None,
                describe: "How `command` runs. shell, the default, types it into your interactive \
                           shell once the pane is ready, so your rc files, aliases and PATH all \
                           apply, and the pane stays a shell afterwards. exec runs it as \
                           `$SHELL -c`, which reads no interactive rc file, ends the pane's \
                           command when it ends, and keeps its exit status for zellij_wait_for \
                           until=exit.",
                default: Some(invoke::RUN_IN_DEFAULT),
            },
            ParamSpec {
                name: "wait_ready",
                kind: ParamKind::Bool,
                required: false,
                from: Some(("action new-pane", "--wait-ready")),
                describe: "Wait until the new pane's shell is ready for input - it drew a prompt, \
                           or drew something and fell quiet - before answering and before typing \
                           `command`. The answer says `ready_by` (prompt or quiet) and \
                           `ready_ms`. Gives up after 10 seconds. Applies when run_in is shell; \
                           false types at once, into a shell that may not be listening yet.",
                default: Some("true"),
            },
            ParamSpec {
                name: "cwd",
                kind: ParamKind::Str,
                required: false,
                from: Some(("action new-pane", "--cwd")),
                describe: "The directory the pane starts in. `~`, `$VAR` and a relative path are \
                           expanded against this server's own environment; a path that is not a \
                           directory fails the call rather than being ignored.",
                default: None,
            },
            ParamSpec {
                name: "name",
                kind: ParamKind::Str,
                required: false,
                from: None,
                describe: "With kind=tab only: the new tab's name. Without one zellij numbers \
                           the tab. Any other kind refuses it; a pane keeps its generated handle.",
                default: None,
            },
            ParamSpec {
                name: "reopen",
                kind: ParamKind::Str,
                required: false,
                from: None,
                describe: "Bring back a closed pane instead of making a new one: `last` for the \
                           newest, or the handle it had. It returns stacked in its old tab (a new \
                           tab if that one closed), in its directory, under its old handle when \
                           free. An agent's resume, or a command pane's command, is typed at the \
                           prompt and not run. Takes no other parameter but session.",
                default: None,
            },
        ],
        read_only: false,
        destructive: false,
        idempotent: false,
    },
    ToolSpec {
        name: "zellij_close",
        summary: "Close a pane or a tab. This cannot be undone.",
        best_for: "clearing away a pane you are done with, or one whose command failed - close it \
                   rather than leaving it and making another.",
        not_for: "a pane somebody else made. Closing a tab closes every pane in it.",
        follow_up: Some("zellij_overview to see what the session has left"),
        reports: &["action close-pane", "action close-tab-by-id"],
        tips: "close_pane passes the `--yes` a person at a terminal would have to type. close_tab \
               has no confirmation to pass and takes every pane in the tab with it. Both are \
               final. Closing a pane whose command failed is the right move - do not leave it \
               behind and create a second one.",
        params: &[
            SESSION,
            ParamSpec {
                name: "operation",
                kind: ParamKind::Enum(&["close_pane", "close_tab"]),
                required: true,
                from: None,
                describe: "What to close. close_pane takes pane; close_tab takes tab.",
                default: None,
            },
            ParamSpec {
                name: "pane",
                kind: ParamKind::Str,
                required: false,
                from: Some(("action close-pane", "--pane-id")),
                describe: "The pane to close: its handle, like sunny-otter, or terminal_1, \
                           plugin_2, a bare integer or a pane uuid.",
                default: None,
            },
            ParamSpec {
                name: "tab",
                kind: ParamKind::Int,
                required: false,
                from: Some(("action close-tab-by-id", "id")),
                describe: "The tab to close, by the stable id zellij_overview prints - not its \
                           1-based display position.",
                default: None,
            },
        ],
        read_only: false,
        destructive: true,
        idempotent: false,
    },
];

/// The tool by that name, or `None`.
pub fn tool_spec(name: &str) -> Option<&'static ToolSpec> {
    TOOLS.iter().find(|tool| tool.name == name)
}

/// Every tool, as the protocol describes them.
pub fn tool_list() -> Vec<Tool> {
    TOOLS.iter().map(describe).collect()
}

/// Where the table names `zellij_wait_for`'s own default wait, which the config's
/// `mcp_wait_timeout` sets: in the tips, and as `timeout_s`'s default. Filled in when a tool is
/// described, so the description promises the bound the command line is given.
pub const WAIT_TIMEOUT_PLACEHOLDER: &str = "{wait_timeout_s}";

/// Every `reason` an answer can carry, in one place.
///
/// The output schema's enum is built from these lists, and the code that puts a reason on an
/// answer names one of these constants rather than a string. A client that validates
/// `structuredContent` drops an answer whose reason the enum lacks, and it did, twice:
/// `every_reason_the_code_writes_is_a_constant_the_schema_lists` reads the source and fails on a
/// reason written as a bare literal.
pub mod reason {
    /// Exit 2: a well-formed request about something that is not there.
    pub const MISS: &str = "miss";
    /// The CLI ran and failed.
    pub const ERROR: &str = "error";
    /// The call could not be turned into a command line.
    pub const BAD_ARGUMENTS: &str = "bad_arguments";
    /// The command line could not be run at all.
    pub const NOT_RUN: &str = "not_run";
    /// The other machine's zellij has no `send-message`.
    pub const REMOTE_TOO_OLD: &str = "remote_too_old";
    /// A create whose pane was made, but whose shell was not ready in time.
    pub const NOT_READY: &str = "not_ready";

    /// Every reason this server writes itself.
    pub const ALL: &[&str] = &[
        MISS,
        ERROR,
        BAD_ARGUMENTS,
        NOT_RUN,
        REMOTE_TOO_OLD,
        NOT_READY,
    ];

    /// Why a receiving session holds or refused a message: `agent_mail::Reason`, which the
    /// record of a `zellij_send` carries up to the top of the answer.
    pub const HELD: &[&str] = &[
        "working",
        "blocked",
        "draft",
        "typing",
        "starting",
        "not_agent",
        "no_state",
        "pane_gone",
    ];
}

/// The shape of every tool's `structuredContent`, which is the same for all seven.
///
/// One schema rather than seven, because every tool is the same child process reported the same
/// way: how the CLI exited, what it printed - parsed when it printed JSON, carried as lines when
/// it did not - and, on a failure, whether it was a miss or an error. The per-tool part is the
/// `result` payload, whose shape the `Returns:` line already describes in the words the surface
/// map uses, so pinning it here would be a second copy of it to keep in step.
fn output_schema() -> JsonObject {
    let mut properties = Map::new();
    properties.insert(
        "exit_code".to_owned(),
        json!({
            "type": "integer",
            "description": "The CLI's exit status: 0 acted or found, 1 an error, 2 a miss - a \
                            well-formed request about something that is not there.",
        }),
    );
    properties.insert(
        "result".to_owned(),
        json!({
            "description": "What the command printed, when it printed JSON. Its shape is the one \
                            the tool's Returns line describes.",
        }),
    );
    properties.insert(
        "output".to_owned(),
        json!({
            "type": "string",
            "description": "What the command printed, when it did not print JSON.",
        }),
    );
    properties.insert(
        "diagnostics".to_owned(),
        json!({
            "type": "string",
            "description": "Anything the command wrote to stderr.",
        }),
    );
    properties.insert(
        "reason".to_owned(),
        json!({
            "type": "string",
            "enum": reason::ALL
                .iter()
                // zellij_send: why the receiving session holds or refused the message
                .chain(reason::HELD.iter())
                .collect::<Vec<_>>(),
            "description": "On a failed call, which kind it was. not_ready is a zellij_create \
                            whose pane was made but whose shell was not ready in time. On \
                            zellij_send, why the message is held or was refused.",
        }),
    );
    let mut schema = Map::new();
    schema.insert("type".to_owned(), json!("object"));
    schema.insert("properties".to_owned(), Value::Object(properties));
    schema.insert("required".to_owned(), json!([]));
    schema
}

/// One tool, description and schema alike.
fn describe(spec: &'static ToolSpec) -> Tool {
    let wait_timeout_s = invoke::wait_timeout_s();
    let mut tool = Tool::new(
        Cow::Borrowed(spec.name),
        Cow::Owned(description_with(spec, wait_timeout_s)),
        Arc::new(input_schema_with(spec, wait_timeout_s)),
    )
    .with_raw_output_schema(Arc::new(output_schema()));
    tool.annotations = Some(
        ToolAnnotations::new()
            .read_only(spec.read_only)
            .destructive(spec.destructive)
            .idempotent(spec.idempotent)
            // every answer comes from a live session on this machine, which is state this server
            // does not own
            .open_world(true),
    );
    tool
}

/// The description a client routes on.
///
/// Four of the five lines are written in the table above. `Returns:` is the fifth, and it is
/// generated from the surface map so that it says what the command actually prints.
#[cfg(test)]
pub fn description(spec: &ToolSpec) -> String {
    description_with(spec, invoke::WAIT_TIMEOUT_DEFAULT_S)
}

/// [`description`], naming `wait_timeout_s` as `zellij_wait_for`'s default wait.
pub fn description_with(spec: &ToolSpec, wait_timeout_s: u64) -> String {
    let mut out = String::from(spec.summary);
    out.push_str("\n\n");
    out.push_str(&format!("Best for: {}\n", spec.best_for));
    out.push_str(&format!("Returns: {}\n", returns_for(spec)));
    out.push_str(&format!("Not for: {}\n", spec.not_for));
    if !spec.tips.is_empty() {
        let tips = spec
            .tips
            .replace(WAIT_TIMEOUT_PLACEHOLDER, &wait_timeout_s.to_string());
        out.push_str(&format!("Notes: {}\n", tips));
    }
    if let Some(follow_up) = spec.follow_up {
        out.push_str(&format!("Follow up with {}.\n", follow_up));
    }
    out
}

/// What a tool puts out, across every operation it multiplexes.
///
/// One command gets one sentence. Several get one sentence per distinct answer, named by the
/// commands that give it, because a tool whose `restore` rebuilds a session and whose `list` prints
/// a table cannot honestly promise the table for both - and a client that was promised columns and
/// handed a payload has been lied to by the description it routed on.
pub fn returns_for(spec: &ToolSpec) -> String {
    match spec.reports {
        [] => "nothing.".to_owned(),
        [only] => returns_line(only),
        many => {
            // commands that answer the same way are said once, together. Four verbs that each
            // print nothing used to spend four sentences saying so, in a line a client routes on
            let mut said: Vec<(String, Vec<&str>)> = Vec::new();
            for command in many {
                let line = returns_line(command);
                match said.iter_mut().find(|(seen, _)| *seen == line) {
                    Some((_, commands)) => commands.push(command),
                    None => said.push((line, vec![command])),
                }
            }
            let mut out = String::from("it depends on the operation.");
            for (line, commands) in said {
                let names = commands
                    .iter()
                    .map(|command| format!("`zellij {}`", command))
                    .collect::<Vec<_>>()
                    .join(", ");
                let verb = if commands.len() == 1 {
                    "returns"
                } else {
                    "return"
                };
                out.push_str(&format!(" {} {} {}", names, verb, line));
            }
            out
        },
    }
}

/// What a command puts out, said in a sentence, from the shape and keys the surface map records.
///
/// Not written by hand anywhere: a column added to a table appears here on the next build, and a
/// command whose row says it prints nothing says so rather than promising a payload.
pub fn returns_line(command: &str) -> String {
    let shape = cli_surface::promised_output_shape(command);
    let keys = cli_surface::promised_output_keys(command);
    match (shape, keys) {
        (Some("table"), Some(keys)) => format!(
            "a table, one row per result, with the columns {}. Structured as JSON where the \
             command offers it.",
            normalize(keys)
        ),
        (Some("record"), Some(keys)) => {
            format!("a record of {}.", normalize(keys))
        },
        (Some("outline"), Some(keys)) => format!("an indented outline of {}.", normalize(keys)),
        (Some("payload"), _) => "the payload itself, and nothing around it.".to_owned(),
        (Some(shape), _) => format!("a {}.", shape),
        (None, _) => {
            "nothing when it succeeds - the fork's convention for a command that only acts."
                .to_owned()
        },
    }
}

/// The keys of a row as written in the surface map, which wraps them across source lines.
fn normalize(keys: &str) -> String {
    keys.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The tool's JSON Schema, with each property described by whatever describes the flag it stands
/// for.
#[cfg(test)]
pub fn input_schema(spec: &ToolSpec) -> JsonObject {
    input_schema_with(spec, invoke::WAIT_TIMEOUT_DEFAULT_S)
}

/// [`input_schema`], with `wait_timeout_s` as `zellij_wait_for`'s `timeout_s` default.
pub fn input_schema_with(spec: &ToolSpec, wait_timeout_s: u64) -> JsonObject {
    let mut properties = Map::new();
    let mut required: Vec<Value> = Vec::new();
    let wait_timeout = wait_timeout_s.to_string();
    for param in spec.params {
        properties.insert(param.name.to_owned(), property(param, &wait_timeout));
        if param.required {
            required.push(json!(param.name));
        }
    }
    let mut schema = Map::new();
    schema.insert("type".to_owned(), json!("object"));
    schema.insert("properties".to_owned(), Value::Object(properties));
    schema.insert("required".to_owned(), Value::Array(required));
    schema.insert("additionalProperties".to_owned(), json!(false));
    schema
}

fn property(param: &ParamSpec, wait_timeout: &str) -> Value {
    let mut property = Map::new();
    match param.kind {
        ParamKind::Str => {
            property.insert("type".to_owned(), json!("string"));
        },
        ParamKind::Bool => {
            property.insert("type".to_owned(), json!("boolean"));
        },
        ParamKind::Int => {
            property.insert("type".to_owned(), json!("integer"));
        },
        ParamKind::Enum(values) => {
            property.insert("type".to_owned(), json!("string"));
            property.insert("enum".to_owned(), json!(values));
        },
    }
    property.insert("description".to_owned(), json!(param_description(param)));
    if let Some(default) = param.default {
        let default = if default == WAIT_TIMEOUT_PLACEHOLDER {
            wait_timeout
        } else {
            default
        };
        // the table writes every default as a string, because that is what a command line takes.
        // An integer property whose `default` is a string is not valid against its own schema, so
        // it is put back into the type the property declares
        let default = match param.kind {
            ParamKind::Int => default
                .parse::<i64>()
                .map(|number| json!(number))
                .unwrap_or_else(|_| json!(default)),
            ParamKind::Bool => default
                .parse::<bool>()
                .map(|flag| json!(flag))
                .unwrap_or_else(|_| json!(default)),
            _ => json!(default),
        };
        property.insert("default".to_owned(), default);
    }
    Value::Object(property)
}

/// A property's description: clap's own help for the flag it stands for, or the hand-written line
/// for a property the CLI has no flag for.
pub fn param_description(param: &ParamSpec) -> String {
    if !param.describe.is_empty() {
        return param.describe.to_owned();
    }
    match param.from {
        Some((command, arg)) => cli_surface::surface_command(command)
            .and_then(|command| command.arg(arg).map(|arg| arg.about.clone()))
            .filter(|about| !about.is_empty())
            // unreachable while `every_parameter_names_a_real_argument` passes; a sentence rather
            // than an empty description if it ever is not
            .unwrap_or_else(|| format!("As `zellij {} {}`.", command, arg)),
        None => param.describe.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_reason_a_held_message_reports_fits_the_output_schema() {
        use zellij_utils::agent_mail::Reason;
        let schema = super::output_schema();
        let allowed = schema["properties"]["reason"]["enum"].as_array().unwrap();
        for reason in [
            Reason::Working,
            Reason::Blocked,
            Reason::Draft,
            Reason::Typing,
            Reason::Starting,
            Reason::NotAgent,
            Reason::NoState,
            Reason::PaneGone,
        ] {
            assert!(
                allowed.iter().any(|v| v == reason.as_str()),
                "{} is missing from the reason enum",
                reason.as_str()
            );
        }
        for reason in super::reason::ALL {
            assert!(
                allowed.iter().any(|v| v == reason),
                "{} is missing from the reason enum",
                reason
            );
        }
    }

    /// The string literals in `text` that are values rather than object keys.
    fn value_literals(text: &str) -> Vec<String> {
        let mut found = Vec::new();
        let mut chars = text.char_indices().peekable();
        while let Some((start, c)) = chars.next() {
            if c != '"' {
                continue;
            }
            let mut literal = String::new();
            let mut end = start;
            while let Some((at, c)) = chars.next() {
                end = at;
                match c {
                    '\\' => {
                        chars.next();
                    },
                    '"' => break,
                    c => literal.push(c),
                }
            }
            let is_key = text[end + 1..].trim_start().starts_with(':');
            if !is_key {
                found.push(literal);
            }
        }
        found
    }

    #[test]
    fn every_reason_the_code_writes_is_a_constant_the_schema_lists() {
        // `not_ready` was written as a literal the enum lacked, and a client that validates the
        // answer hid the handle of a pane that exists. Every reason now goes through
        // `reason::*`, which the enum is built from; a literal beside `"reason"` fails here
        let sources = [
            ("mod.rs", include_str!("mod.rs")),
            ("invoke.rs", include_str!("invoke.rs")),
            ("send.rs", include_str!("send.rs")),
            ("fleet.rs", include_str!("fleet.rs")),
            ("remote.rs", include_str!("remote.rs")),
        ];
        let mut sites = 0;
        for (file, source) in sources {
            let code = source
                .split("\n#[cfg(test)]\nmod tests")
                .next()
                .unwrap_or(source);
            for (at, _) in code.match_indices("\"reason\"") {
                let rest = &code[at + "\"reason\"".len()..];
                let statement = &rest[..rest.find(';').unwrap_or(rest.len())];
                let literals = value_literals(statement);
                assert!(
                    literals.is_empty(),
                    "{} writes the reason {:?} as a literal: name a `reason::` constant, \
                     and add it to `reason::ALL` if it is new",
                    file,
                    literals,
                );
                if statement.contains("reason::") {
                    sites += 1;
                }
            }
        }
        // the scan found the code it guards, rather than passing on nothing
        assert!(sites >= 10, "found only {} reason sites", sites);
        let schema = super::output_schema();
        let allowed = schema["properties"]["reason"]["enum"].as_array().unwrap();
        assert!(allowed.iter().any(|v| v == "not_ready"));
    }

    #[test]
    fn a_literal_beside_a_reason_is_caught() {
        assert_eq!(
            value_literals(r#": "not_ready"));"#),
            vec!["not_ready".to_owned()]
        );
        assert!(value_literals(r#": reason::MISS, "from": address})"#).is_empty());
        assert_eq!(
            value_literals(r#"json!(if miss { "miss" } else { reason::ERROR })"#),
            vec!["miss".to_owned()]
        );
    }

    use super::*;

    #[test]
    fn every_parameter_names_a_real_argument() {
        for tool in TOOLS {
            for param in tool.params {
                let Some((command, arg)) = param.from else {
                    continue;
                };
                let found = cli_surface::surface_command(command).unwrap_or_else(|| {
                    panic!(
                        "{} names `zellij {}`, which is not a command",
                        tool.name, command
                    )
                });
                assert!(
                    found.arg(arg).is_some(),
                    "{}'s `{}` names `{}` of `zellij {}`, which has no such argument",
                    tool.name,
                    param.name,
                    arg,
                    command,
                );
            }
        }
    }

    #[test]
    fn every_generated_description_is_the_flags_own_help() {
        for tool in TOOLS {
            for param in tool.params {
                let Some((command, arg)) = param.from else {
                    continue;
                };
                let expected = cli_surface::surface_command(command)
                    .and_then(|command| command.arg(arg).map(|arg| arg.about.clone()))
                    .expect("the argument exists");
                assert!(
                    !expected.is_empty(),
                    "`{}` of `zellij {}` has no help for {} to borrow",
                    arg,
                    command,
                    param.name,
                );
                if !param.describe.is_empty() {
                    // a property that says what it is in the caller's terms keeps saying it; the
                    // flag it names is still checked, above and in the test before this one
                    continue;
                }
                assert_eq!(
                    param_description(param),
                    expected,
                    "{}'s `{}` does not carry the help of `{}`",
                    tool.name,
                    param.name,
                    arg,
                );
            }
        }
    }

    #[test]
    fn a_property_written_for_the_caller_beats_the_help_written_for_a_command_line() {
        // clap's help describes an argv: `--` positions, `-` for stdin, "the focused pane". None of
        // those exist for a caller passing JSON, and every one of them was in the rendered schema
        for (tool, param) in [
            ("zellij_write_input", "keys"),
            ("zellij_write_input", "text"),
            ("zellij_read_pane", "pane"),
            ("zellij_create", "command"),
        ] {
            let spec = tool_spec(tool).expect("a tool");
            let param = spec
                .params
                .iter()
                .find(|candidate| candidate.name == param)
                .expect("a parameter");
            let description = param_description(param);
            assert_eq!(description, param.describe, "{} of {}", param.name, tool);
            for argv_ism in ["`--`", "after a --", "stdin", "--focused", "see above"] {
                assert!(
                    !description.contains(argv_ism),
                    "{} of {} still says `{}`",
                    param.name,
                    tool,
                    argv_ism,
                );
            }
        }
    }

    #[test]
    fn every_returns_line_is_the_surface_maps_own_keys() {
        for tool in TOOLS {
            assert!(!tool.reports.is_empty(), "{} reports nothing", tool.name);
            let spec_line = returns_for(tool);
            for command in tool.reports {
                let line = returns_line(command);
                assert!(
                    cli_surface::surface_command(command).is_some(),
                    "{} reports `zellij {}`, which is not a command",
                    tool.name,
                    command,
                );
                if cli_surface::promised_output_shape(command).is_none() {
                    // a verb that only acts, and a Returns line that says exactly that
                    assert!(line.starts_with("nothing when it succeeds"), "{}", line);
                }
                if let Some(keys) = cli_surface::promised_output_keys(command) {
                    for key in keys.split_whitespace() {
                        assert!(
                            line.contains(key),
                            "{}'s Returns line drops the `{}` key of `zellij {}`",
                            tool.name,
                            key,
                            command,
                        );
                    }
                }
                // and every operation's shape reaches the description the client routes on
                assert!(
                    spec_line.contains(&line),
                    "{}'s description drops what `zellij {}` returns",
                    tool.name,
                    command,
                );
            }
        }
    }

    #[test]
    fn a_tool_that_multiplexes_says_what_each_operation_returns() {
        // `list-panes` and `list-agents` print tables of different columns and `list-sessions`
        // prints its own; promising one shape for all three is the kind of lie a client cannot
        // detect
        let overview = tool_spec("zellij_overview").expect("a tool");
        let line = returns_for(overview);
        assert!(line.starts_with("it depends on the operation"), "{}", line);
        assert!(line.contains("action list-panes"), "{}", line);
        assert!(line.contains("action list-agents"), "{}", line);
    }

    #[test]
    fn a_read_only_tool_is_idempotent() {
        for tool in TOOLS {
            if tool.read_only {
                assert!(
                    tool.idempotent,
                    "{} reads only, so repeating it cannot change anything",
                    tool.name
                );
            }
        }
    }

    #[test]
    fn what_can_be_undone_is_gated_apart_from_what_cannot() {
        // the reason this server exists is that a client gates verbs one by one, so the closes
        // live in one destructive tool and no other tool offers one
        let close = tool_spec("zellij_close").expect("a tool");
        assert!(close.destructive, "closing cannot be undone");
        for spec in TOOLS {
            for param in spec.params {
                let ParamKind::Enum(operations) = param.kind else {
                    continue;
                };
                for operation in operations {
                    assert_eq!(
                        operation.starts_with("close_"),
                        spec.name == "zellij_close",
                        "{} offers {}",
                        spec.name,
                        operation,
                    );
                }
            }
        }
    }

    #[test]
    fn every_tool_declares_the_shape_of_what_it_returns() {
        for tool in tool_list() {
            let schema = tool.output_schema.as_ref().expect("an output schema");
            assert_eq!(schema.get("type"), Some(&json!("object")));
            let properties = schema
                .get("properties")
                .and_then(|properties| properties.as_object())
                .expect("properties");
            assert!(properties.contains_key("exit_code"), "{}", tool.name);
            assert!(properties.contains_key("result"), "{}", tool.name);
        }
    }

    #[test]
    fn a_table_of_columns_is_reported_as_its_columns() {
        // the drift gate in miniature: `list-panes` gained an AGENT column, and the line says so
        // without anybody editing a description
        let line = returns_line("action list-panes");
        assert!(line.starts_with("a table"), "{}", line);
        assert!(line.contains("HANDLE"), "{}", line);
        assert!(line.contains("AGENT"), "{}", line);
    }

    #[test]
    fn a_command_that_prints_nothing_is_not_dressed_up_as_one_that_does() {
        let line = returns_line("action move-focus");
        assert!(line.starts_with("nothing when it succeeds"), "{}", line);
    }

    #[test]
    fn every_tool_says_what_it_is_not_for_and_what_comes_next() {
        for tool in TOOLS {
            let description = description(tool);
            assert!(
                description.contains("Best for:"),
                "{} has no Best for line",
                tool.name
            );
            assert!(
                description.contains("Not for:"),
                "{} has no Not for line",
                tool.name
            );
            assert!(
                description.contains("Returns:"),
                "{} has no Returns line",
                tool.name
            );
        }
    }

    #[test]
    fn the_surface_stays_small_and_the_names_do_not_collide() {
        assert!(
            TOOLS.len() <= 7,
            "the point of this server is a surface an agent can route through"
        );
        let mut names: Vec<&str> = TOOLS.iter().map(|tool| tool.name).collect();
        names.sort();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "two tools answer to the same name");
        for tool in TOOLS {
            assert!(
                tool.name.starts_with("zellij_"),
                "{} is not namespaced by its service",
                tool.name
            );
            // Keep each tool's argument list small enough to use reliably
            assert!(
                tool.params.len() <= 9,
                "{} asks for more parameters than an agent can be expected to get right",
                tool.name
            );
        }
    }

    #[test]
    fn create_schema_offers_only_stacks_and_tabs() {
        let create = TOOLS
            .iter()
            .find(|tool| tool.name == "zellij_create")
            .unwrap();
        let schema = input_schema(create);
        assert_eq!(
            schema["properties"]["kind"]["enum"],
            json!(["agent_tab", "stacked", "tab"])
        );
        assert!(schema["properties"].get("floating").is_none());
        assert_eq!(invoke::CREATE_KIND_DEFAULT, "agent_tab");
    }

    #[test]
    fn every_schema_is_an_object_that_lists_its_required_properties() {
        for tool in TOOLS {
            let schema = input_schema(tool);
            assert_eq!(schema.get("type"), Some(&json!("object")), "{}", tool.name);
            let properties = schema
                .get("properties")
                .and_then(|p| p.as_object())
                .expect("properties");
            let required = schema
                .get("required")
                .and_then(|r| r.as_array())
                .expect("required");
            for param in tool.params {
                assert!(properties.contains_key(param.name), "{}", param.name);
                assert_eq!(
                    required.contains(&json!(param.name)),
                    param.required,
                    "{} of {}",
                    param.name,
                    tool.name
                );
            }
        }
    }

    #[test]
    fn a_read_only_tool_is_not_also_a_destructive_one() {
        for tool in TOOLS {
            if tool.read_only {
                assert!(
                    !tool.destructive,
                    "{} claims to change nothing and to destroy something",
                    tool.name
                );
            }
        }
    }

    #[test]
    fn every_key_the_write_input_tool_names_is_one_the_key_parser_takes() {
        use std::str::FromStr;
        use zellij_utils::data::KeyWithModifier;
        // the description used to teach `C-c` and `Escape`, and the CLI refused both. Every
        // backticked span in what the tool says about keys is a key, except the spellings it
        // names to warn against, which have to stay refused for the warning to be true
        let not_keys = ["C-c", "Ctrl-c"];
        let tool = TOOLS
            .iter()
            .find(|tool| tool.name == "zellij_write_input")
            .unwrap();
        let keys = tool.params.iter().find(|p| p.name == "keys").unwrap();
        let said = format!("{} {}", tool.tips, param_description(keys));
        let spans: Vec<&str> = said.split('`').skip(1).step_by(2).collect();
        assert!(spans.len() >= 4, "found no examples in: {}", said);
        for span in spans {
            let parsed = KeyWithModifier::from_str(span);
            if not_keys.contains(&span) {
                assert!(
                    parsed.is_err(),
                    "`{}` is named as not a key, but parses",
                    span
                );
            } else {
                assert!(
                    parsed.is_ok(),
                    "`{}` is offered as a key, but does not parse",
                    span
                );
            }
        }
    }
}
