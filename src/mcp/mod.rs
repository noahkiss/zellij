//! `zellij mcp`: the CLI's surface, served to an agent over the Model Context Protocol.
//!
//! Why this exists when `zellij setup --dump-surface` already describes the whole command tree in
//! one call: a harness that cannot shell out cannot use the CLI at all, and one that can still has
//! to be told, per call, whether a verb is allowed. MCP answers both - the tools ARE the surface,
//! and a client gates them one by one.
//!
//! Three things keep it honest, and each is enforced somewhere rather than promised here:
//!
//! * **Nine tools, not eighty-seven.** The table in [`tools`] is the whole surface, and a test
//!   fails if it grows past nine or if a tool asks for more than ten parameters.
//! * **The descriptions are generated.** What a tool returns, and what each of its parameters
//!   means, come out of the same map `--dump-surface` reads. A renamed flag fails the build.
//! * **The tools run the CLI.** Every call is a child process of the binary at this one's path, so
//!   a tool behaves exactly as the command line does, misses included. See [`invoke`].
//!
//! Session lifecycle - `session up`, `down`, `restart`, `enable` - is deliberately absent. Those
//! start and stop the thing this server is talking to, and they stay where a person runs them.

pub mod invoke;
pub mod tools;

use std::future::Future;

use rmcp::model::{
    CacheScope, CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ErrorData,
    Implementation, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::{RoleServer, ServerHandler, ServiceExt};
use serde_json::{json, Map, Value};
use zellij_utils::consts::VERSION;

/// What a client is told this server is, before it asks for anything.
const INSTRUCTIONS: &str = "\
Drive a running zellij session: read what is on a pane, wait for something to happen in one, type
into one, and make or rearrange panes and tabs.

Start with zellij_overview. Panes are addressed by their two-word handle - `sunny-otter` - which is
the pane's address, survives a session restore, and is what every other tool wants. An integer id
works too.

Which session a call is about: the tool's own `session` argument, or the session this server was
started inside. Nothing here starts or stops a session; that is the command line's job.

Work in ONE pane. Before you create anything, call zellij_overview and look for a pane you already
own - its handle is in that answer. Have one? Use zellij_write_input, not zellij_create. Did a
command fail? Reuse that pane, or close it with zellij_close. Do not answer a failure by making
another pane. A person is watching this terminal, and a session that grows a pane per command is
one they cannot read.

zellij_create puts your pane in your own tab: the tab you are in with `-zj` on the end, made the
first time and reused after. It does not take the person's focus. That keeps your work in one
place and out of the pane they are looking at.

zellij_overview with scope=agents answers which panes are running a coding agent, and which agent
each one is - that is how you find the pane to talk to without being told its name.

zellij_show puts image files in front of the person, in a pop-up pane they close with q.";

/// The server itself. It holds nothing: every answer comes from a child process.
#[derive(Clone, Debug, Default)]
pub struct ZellijMcp {
    /// The session an unqualified call is about, read once at startup.
    ambient_session: Option<String>,
}

impl ZellijMcp {
    pub fn new(session: Option<String>) -> Self {
        ZellijMcp {
            ambient_session: session.or_else(invoke::ambient_session),
        }
    }

    /// One tool call: build the command line, run it, and report what the CLI said.
    async fn call(&self, name: &str, arguments: Map<String, Value>) -> CallToolResult {
        let plan = match invoke::plan(name, &arguments, self.ambient_session.as_deref()).await {
            Ok(plan) => plan,
            // a call that could not be turned into a command line never ran, and the caller is
            // told what was missing rather than being handed an empty result
            Err(message) => return failed(message, json!({"reason": "bad_arguments"})),
        };
        let then_type = plan.then_type;
        let (argv, mut outcome) = match self.run_plan(plan.call).await {
            Ok(ran) => ran,
            Err(message) => return failed(message, json!({"reason": "not_run"})),
        };
        let input_sent = match then_type {
            Some(then_type) => Some(type_into_created_pane(then_type, &mut outcome).await),
            None => None,
        };
        self.report(&argv, outcome, input_sent)
    }

    /// Run whatever the plan turned out to be, and say which command line answered.
    ///
    /// One create, always: whether the agent's tab is there was answered from the pane list before
    /// the command line was built, so nothing here reads an exit code to decide what to run next.
    /// The second command line a create can carry is the rename, which `--new-tab` cannot do
    /// itself.
    async fn run_plan(&self, plan: invoke::Call) -> Result<(Vec<String>, invoke::Outcome), String> {
        let (argv, rename) = match plan {
            invoke::Call::One(argv) => {
                let outcome = invoke::run(&argv).await?;
                return Ok((argv, outcome));
            },
            invoke::Call::Sequence(argvs) => {
                let mut ran = None;
                for (step, argv) in argvs.into_iter().enumerate() {
                    if step > 0 {
                        tokio::time::sleep(std::time::Duration::from_millis(
                            invoke::SEQUENCE_PAUSE_MS,
                        ))
                        .await;
                    }
                    let outcome = invoke::run(&argv).await?;
                    let failed = outcome.is_error();
                    ran = Some((argv, outcome));
                    if failed {
                        break;
                    }
                }
                return ran.ok_or_else(|| "Nothing to run.".to_owned());
            },
            invoke::Call::MakingAgentTab { argv, rename } => (argv, rename),
        };
        let mut outcome = invoke::run(&argv).await?;
        // `--new-tab` refuses `--name`, so the pane that arrived with the tab is named now. A
        // rename that fails is a pane with the wrong title, not a create that did not happen: it
        // is reported beside the create rather than instead of it, and a rename that could not be
        // run at all says so rather than vanishing
        if let (Some(rename), Some(pane)) = (rename, invoke::reported_pane_id(&outcome.stdout)) {
            match invoke::run(&rename.argv(&pane)).await {
                Ok(renamed) if renamed.is_error() => {
                    also_said(&mut outcome.stderr, &renamed.stderr)
                },
                Ok(_) => {},
                Err(message) => also_said(&mut outcome.stderr, &message),
            }
        }
        Ok((argv, outcome))
    }

    /// What the CLI said, as the protocol carries it.
    ///
    /// `input_sent` is a shell-mode create's second half: whether its command was typed into the
    /// pane. `None` for every call that had nothing to type.
    fn report(
        &self,
        argv: &[String],
        outcome: invoke::Outcome,
        input_sent: Option<bool>,
    ) -> CallToolResult {
        let mut structured = Map::new();
        structured.insert("exit_code".to_owned(), json!(outcome.code));
        for (key, value) in invoke::call_context(argv) {
            structured.insert(key, json!(value));
        }
        // a create that waited for its pane says how it ended, whichever way that was: the two
        // lines the CLI added, and `ready` itself, since a pane that never got them is not ready
        let waited = argv.iter().any(|word| word == "--wait-ready");
        if waited {
            let ready_by = report_value(&outcome.stdout, "ready_by");
            structured.insert("ready".to_owned(), json!(ready_by.is_some()));
            if let Some(ready_by) = ready_by {
                structured.insert("ready_by".to_owned(), json!(ready_by));
            }
            if let Some(ready_ms) = report_value(&outcome.stdout, "ready_ms")
                .and_then(|ready_ms| ready_ms.parse::<u64>().ok())
            {
                structured.insert("ready_ms".to_owned(), json!(ready_ms));
            }
        }
        if let Some(input_sent) = input_sent {
            structured.insert("input_sent".to_owned(), json!(input_sent));
        }
        // a create that waited, missed, and still named its pane made a pane whose shell was not
        // ready in time. That is not the "nothing matched" every other miss is: the pane is there,
        // and the caller made it and has to close it
        if waited && outcome.is_miss() && invoke::reported_pane_id(&outcome.stdout).is_some() {
            // nothing was typed into a shell that had not said it was listening
            structured.insert("reason".to_owned(), json!("not_ready"));
            if let Some(pane_id) = report_value(&outcome.stdout, "pane_id") {
                structured.insert("pane_id".to_owned(), json!(pane_id));
            }
            if let Some(handle) = report_value(&outcome.stdout, "handle") {
                structured.insert("handle".to_owned(), json!(handle));
            }
            structured.insert("output".to_owned(), json!(outcome.stdout));
            if !outcome.stderr.trim().is_empty() {
                structured.insert("diagnostics".to_owned(), json!(outcome.stderr.trim()));
            }
            let message = format!(
                "The pane was made, but its shell was not ready within {} seconds{}. {} Write to \
                 the pane once it is ready, or close it.\n{}",
                invoke::READY_TIMEOUT_DEFAULT_S,
                if input_sent.is_some() {
                    ", so the command was not typed into it"
                } else {
                    ""
                },
                first_words(&outcome.stderr, ""),
                outcome.stdout.trim()
            );
            return with_structure(
                CallToolResult::error(vec![ContentBlock::text(message)]),
                structured,
            );
        }
        // the CLI's JSON answers are parsed back so that a client gets structure rather than a
        // string holding structure; anything else is carried as the lines it printed
        match serde_json::from_str::<Value>(outcome.stdout.trim()) {
            Ok(value) if outcome.stdout.trim_start().starts_with(['{', '[']) => {
                structured.insert("result".to_owned(), value);
            },
            _ => {
                structured.insert("output".to_owned(), json!(outcome.stdout));
            },
        }
        if !outcome.stderr.trim().is_empty() {
            structured.insert("diagnostics".to_owned(), json!(outcome.stderr.trim()));
        }
        if outcome.is_error() {
            // exit 2 is the fork's "well-formed request about something that is not there". It is
            // the reason a create tool can be honest: a pane that was not made says so, and there
            // is no id here to invent
            //
            // A pane privacy policy has no reason of its own here. It answers as the miss answers,
            // to the byte, so that a caller cannot tell a withheld pane from one that was never
            // there - a `withheld` reason would be the oracle the whole filter exists to deny.
            // `zellij_overview` still carries the aggregate count, which is where a caller learns
            // that its view is partial.
            structured.insert(
                "reason".to_owned(),
                json!(if outcome.is_miss() { "miss" } else { "error" }),
            );
            let said = first_words(&outcome.stderr, &outcome.stdout);
            let message = if outcome.is_miss() {
                format!("Nothing matched: {}", said)
            } else {
                said
            };
            return with_structure(
                CallToolResult::error(vec![ContentBlock::text(message)]),
                structured,
            );
        }
        let text = if outcome.stdout.trim().is_empty() {
            format!("`zellij {}` succeeded and printed nothing.", argv.join(" "))
        } else {
            outcome.stdout.clone()
        };
        with_structure(
            CallToolResult::success(vec![ContentBlock::text(text)]),
            structured,
        )
    }
}

/// Type a shell-mode create's command into the pane the create made, and say whether it was.
///
/// Only into a pane the create reported, and only when the create succeeded - which, with
/// `--wait-ready`, is when the pane's shell said it was ready. A create that was not ready in time
/// exits 2 and still names its pane, and nothing is typed into it. A write that fails leaves a pane
/// with nothing running in it, so its failure becomes the call's.
async fn type_into_created_pane(
    then_type: invoke::TypeAfter,
    outcome: &mut invoke::Outcome,
) -> bool {
    let Some(pane) = invoke::reported_pane_id(&outcome.stdout) else {
        return false;
    };
    if outcome.is_error() {
        return false;
    }
    for argv in then_type.argvs(&pane) {
        let failure = match invoke::run(&argv).await {
            Ok(said) if said.is_error() => (said.code, said.stderr),
            Ok(_) => continue,
            Err(message) => (1, message),
        };
        outcome.code = failure.0;
        also_said(&mut outcome.stderr, &failure.1);
        return false;
    }
    true
}

/// The value of one `key: value` line of a CLI report.
fn report_value(stdout: &str, key: &str) -> Option<String> {
    stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix(key)?.strip_prefix(':'))
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// How long a client may treat the tool list as fresh, in milliseconds.
///
/// The list is compiled in - see [`tools::TOOLS`] - so it cannot change while this process is
/// alive, and a client that reaches a new list has by definition reconnected to a new process. An
/// hour is therefore not a guess about staleness; it is a bound on how long a client will hold a
/// list it would get back unchanged anyway.
const TOOL_LIST_TTL_MS: u64 = 60 * 60 * 1000;

/// The tool list, with the cache metadata protocol version `2026-07-28` requires.
///
/// `ttlMs` and `cacheScope` (SEP-2549) are optional in rmcp's `ListToolsResult` because that one
/// type also models results from older protocol versions - so a server that never sets them emits
/// a result the 2026-07-28 schema rejects. Claude Code takes that era through `server/discover`
/// and then refuses the whole list ("ttlMs expected number"), leaving the client connected with no
/// tools at all. Setting the pair is what makes this server conform, rather than opting out of the
/// era by narrowing `supported_protocol_versions`.
fn tool_list_result() -> ListToolsResult {
    ListToolsResult::with_all_items(tools::tool_list())
        .with_ttl_ms(TOOL_LIST_TTL_MS)
        // the narrower of the two scopes: nothing in the list is user-specific today, but a
        // shared cache is worth nothing to a server a client spawns for itself over stdio
        .with_cache_scope(CacheScope::Private)
}

/// A call that failed before, or instead of, reaching the CLI.
fn failed(message: String, structured: Value) -> CallToolResult {
    let structured = structured.as_object().cloned().unwrap_or_default();
    with_structure(
        CallToolResult::error(vec![ContentBlock::text(message)]),
        structured,
    )
}

fn with_structure(mut result: CallToolResult, structured: Map<String, Value>) -> CallToolResult {
    result.structured_content = Some(Value::Object(structured));
    result
}

/// Add what a follow-up command said to what the first one said, on a line of its own.
///
/// Two diagnostics run together read as one sentence that neither command wrote, and the caller
/// gets them as a single `diagnostics` string with no way to split them again.
fn also_said(said: &mut String, line: &str) {
    let line = line.trim();
    if line.is_empty() {
        return;
    }
    let kept = said.trim_end().len();
    said.truncate(kept);
    if !said.is_empty() {
        said.push('\n');
    }
    said.push_str(line);
}

/// What the CLI said, preferring its diagnostics: an error's explanation goes to stderr.
fn first_words(stderr: &str, stdout: &str) -> String {
    let said = if stderr.trim().is_empty() {
        stdout.trim()
    } else {
        stderr.trim()
    };
    if said.is_empty() {
        "The command failed and said nothing.".to_owned()
    } else {
        said.to_owned()
    }
}

impl ServerHandler for ZellijMcp {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::new(ServerCapabilities::builder().enable_tools().build());
        info.server_info = Implementation::new("zellij", VERSION);
        info.instructions = Some(INSTRUCTIONS.to_owned());
        info
    }

    fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<ListToolsResult, ErrorData>> + Send + '_ {
        async move { Ok(tool_list_result()) }
    }

    fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResponse, ErrorData>> + Send + '_ {
        async move {
            let name = request.name.to_string();
            // an unknown tool is a protocol error rather than a failed call: the client asked for
            // something this server never offered
            if tools::tool_spec(&name).is_none() {
                return Err(ErrorData::invalid_params(
                    format!("`{}` is not a tool of this server", name),
                    None,
                ));
            }
            let arguments = request.arguments.unwrap_or_default();
            Ok(CallToolResponse::from(self.call(&name, arguments).await))
        }
    }
}

/// Serve MCP over stdin and stdout until the client goes away.
///
/// Returns the process exit status. Nothing in here may write to stdout: that is the protocol
/// stream, and a stray line would end the session. Diagnostics go to stderr, which is why the
/// tools capture their child process's output rather than letting it through.
pub fn start(session: Option<String>) -> i32 {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("Could not start the MCP server's runtime: {}", e);
            return 1;
        },
    };
    runtime.block_on(async move {
        let service = match ZellijMcp::new(session)
            .serve(rmcp::transport::io::stdio())
            .await
        {
            Ok(service) => service,
            Err(e) => {
                eprintln!(
                    "The MCP client and this server could not agree on a session: {}",
                    e
                );
                return 1;
            },
        };
        match service.waiting().await {
            Ok(_) => 0,
            Err(e) => {
                eprintln!("The MCP session ended: {}", e);
                1
            },
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_server_offers_exactly_the_tools_in_the_table() {
        let listed = tools::tool_list();
        assert_eq!(listed.len(), tools::TOOLS.len());
        for tool in &listed {
            assert!(
                tools::tool_spec(&tool.name).is_some(),
                "{} is listed and cannot be called",
                tool.name
            );
            assert!(
                tool.description.as_ref().is_some_and(|d| !d.is_empty()),
                "{} has no description to route on",
                tool.name
            );
            let annotations = tool.annotations.as_ref().expect("annotations");
            assert!(annotations.read_only_hint.is_some(), "{}", tool.name);
        }
    }

    #[test]
    fn the_tool_list_carries_the_cache_metadata_the_2026_schema_requires() {
        // a client that negotiates 2026-07-28 rejects the whole list when either field is absent,
        // and reports a connected server with no tools rather than a protocol error
        let result = tool_list_result();
        assert_eq!(result.ttl_ms, Some(TOOL_LIST_TTL_MS));
        assert_eq!(result.cache_scope, Some(CacheScope::Private));
        assert_eq!(result.tools.len(), tools::TOOLS.len());

        // and on the wire, where the client actually reads them
        let wire = serde_json::to_value(&result).expect("the tool list serializes");
        assert!(wire["ttlMs"].is_number(), "ttlMs missing: {}", wire);
        assert_eq!(wire["cacheScope"], json!("private"), "{}", wire);
    }

    fn outcome(code: i32, stdout: &str, stderr: &str) -> invoke::Outcome {
        invoke::Outcome {
            code,
            stdout: stdout.to_owned(),
            stderr: stderr.to_owned(),
        }
    }

    fn create_argv() -> Vec<String> {
        "action new-pane --near-current-pane --wait-ready --ready-timeout=10"
            .split(' ')
            .map(str::to_owned)
            .collect()
    }

    #[tokio::test]
    async fn nothing_is_typed_into_a_pane_whose_shell_was_not_ready() {
        // the create named its pane and exited 2: the shell never said it was listening. Nothing
        // runs here - a write would spawn a child, and this returns before one could
        let plan = invoke::plan(
            "zellij_create",
            json!({"kind": "pane", "command": "make"})
                .as_object()
                .unwrap(),
            None,
        )
        .await
        .expect("a plan");
        let mut missed = outcome(
            2,
            "pane_id: terminal_7\nhandle: sunny-otter\n",
            "terminal_7 was not ready when the wait timed out.\n",
        );
        let sent = type_into_created_pane(plan.then_type.unwrap(), &mut missed).await;
        assert!(!sent);
        assert_eq!(missed.code, 2, "the create's own answer stands");
    }

    #[test]
    fn a_pane_that_was_not_ready_is_reported_by_handle_and_as_not_ready() {
        let server = ZellijMcp::default();
        let result = server.report(
            &create_argv(),
            outcome(
                2,
                "pane_id: terminal_7\nhandle: sunny-otter\n",
                "terminal_7 was not ready when the wait timed out.\n",
            ),
            Some(false),
        );
        assert_eq!(result.is_error, Some(true));
        let structured = result.structured_content.unwrap();
        assert_eq!(structured["reason"], json!("not_ready"));
        assert_eq!(structured["ready"], json!(false));
        assert_eq!(structured["input_sent"], json!(false));
        assert_eq!(structured["handle"], json!("sunny-otter"));
        assert_eq!(structured["pane_id"], json!("terminal_7"));
        let text = serde_json::to_string(&result.content).unwrap();
        assert!(text.contains("not typed"), "{}", text);
        assert!(text.contains("sunny-otter"), "{}", text);
        // the negative control: a miss that made no pane is still the ordinary miss
        let result = server.report(
            &create_argv(),
            outcome(2, "", "No tab answers to 'x'.\n"),
            None,
        );
        assert_eq!(result.structured_content.unwrap()["reason"], json!("miss"));
    }

    #[test]
    fn a_ready_pane_says_how_and_how_long_and_whether_its_command_was_typed() {
        let result = ZellijMcp::default().report(
            &create_argv(),
            outcome(
                0,
                "pane_id: terminal_7\nhandle: sunny-otter\nready_by: prompt\nready_ms: 212\n",
                "",
            ),
            Some(true),
        );
        assert_eq!(result.is_error, Some(false));
        let structured = result.structured_content.unwrap();
        assert_eq!(structured["ready"], json!(true));
        assert_eq!(structured["ready_by"], json!("prompt"));
        assert_eq!(structured["ready_ms"], json!(212));
        assert_eq!(structured["input_sent"], json!(true));
        // a call that typed nothing carries no `input_sent` at all
        let result = ZellijMcp::default().report(
            &create_argv(),
            outcome(
                0,
                "pane_id: terminal_7\nready_by: quiet\nready_ms: 300\n",
                "",
            ),
            None,
        );
        assert!(result
            .structured_content
            .unwrap()
            .get("input_sent")
            .is_none());
    }

    #[test]
    fn the_instructions_send_a_reader_to_the_tool_that_starts_a_task() {
        assert!(INSTRUCTIONS.contains("zellij_overview"));
        assert!(tools::tool_spec("zellij_overview").is_some());
    }

    #[test]
    fn session_lifecycle_is_not_reachable() {
        for verb in ["session_up", "session_down", "restart", "kill", "delete"] {
            assert!(
                !tools::TOOLS.iter().any(|tool| tool.name.contains(verb)),
                "{} became a tool; lifecycle stays on the command line",
                verb
            );
        }
    }
}
