//! `zellij mcp`: the CLI's surface, served to an agent over the Model Context Protocol.
//!
//! Why this exists when `zellij setup --dump-surface` already describes the whole command tree in
//! one call: a harness that cannot shell out cannot use the CLI at all, and one that can still has
//! to be told, per call, whether a verb is allowed. MCP answers both - the tools ARE the surface,
//! and a client gates them one by one.
//!
//! Three things keep it honest, and each is enforced somewhere rather than promised here:
//!
//! * **Seven tools, not eighty-seven.** The table in [`tools`] is the whole surface, and a test
//!   fails if it grows past eight or if a tool asks for more than eight parameters.
//! * **The descriptions are generated.** What a tool returns, and what each of its parameters
//!   means, come out of the same map `--dump-surface` reads. A renamed flag fails the build.
//! * **The tools run the CLI.** Every call is a child process of this same binary, so a tool
//!   behaves exactly as the command line does, misses included. See [`invoke`].
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

zellij_overview with scope=agents answers which panes are running a coding agent, and which agent
each one is - that is how you find the pane to talk to without being told its name.";

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
        let argv = match invoke::argv(name, &arguments, self.ambient_session.as_deref()) {
            Ok(argv) => argv,
            // a call that could not be turned into a command line never ran, and the caller is
            // told what was missing rather than being handed an empty result
            Err(message) => return failed(message, json!({"reason": "bad_arguments"})),
        };
        let outcome = match invoke::run(&argv).await {
            Ok(outcome) => outcome,
            Err(message) => return failed(message, json!({"reason": "not_run"})),
        };
        let mut structured = Map::new();
        structured.insert("exit_code".to_owned(), json!(outcome.code));
        for (key, value) in invoke::call_context(&argv) {
            structured.insert(key, json!(value));
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
