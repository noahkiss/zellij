//! `zellij_overview scope=fleet`: every coding agent, in every session, on every listed host.
//!
//! One `list-agents --all-sessions --json` here and one on each remote host, all at once, each
//! bounded by [`remote::FLEET_TIMEOUT_S`]. A host that does not answer is one short row in the
//! answer, never an error for the whole call and never a wait longer than the bound: a
//! supervisor asking "what is running everywhere" wants the hosts that answered now.

use std::time::Duration;

use rmcp::model::{CallToolResult, ContentBlock};
use serde_json::{json, Map, Value};

use super::invoke::{self, AgentsView, Outcome};
use super::remote::{self, RemoteHosts, Target};

/// The command line asked of every host.
pub fn list_agents_argv() -> Vec<String> {
    ["action", "list-agents", "--all-sessions", "--json"]
        .iter()
        .map(|word| (*word).to_owned())
        .collect()
}

/// What one host said, or why it said nothing.
pub type Answer = Result<Outcome, String>;

/// The whole call: ask every host, then merge what came back.
pub async fn overview(hosts: &RemoteHosts, local: &str) -> CallToolResult {
    let mut targets = vec![(hosts.local_name(local), Target::Local)];
    targets.extend(
        hosts
            .remote(local)
            .into_iter()
            .map(|host| (host.name.clone(), Target::Remote(host.clone()))),
    );
    let answers = gather(targets, Duration::from_secs(remote::FLEET_TIMEOUT_S)).await;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64);
    merge(&answers, now_ms, hosts, Some(&hosts.local_name(local)))
}

/// Ask every target at once, each bounded by `timeout`, and return the answers in target order.
pub async fn gather(targets: Vec<(String, Target)>, timeout: Duration) -> Vec<(String, Answer)> {
    let argv = list_agents_argv();
    let mut asking = tokio::task::JoinSet::new();
    for (index, (name, target)) in targets.into_iter().enumerate() {
        let argv = argv.clone();
        asking.spawn(async move {
            // the child is killed when the timed-out future is dropped
            let answer = tokio::time::timeout(timeout, ask(&target, &argv))
                .await
                .unwrap_or_else(|_| Err(format!("no answer within {} seconds", timeout.as_secs())));
            (index, name, answer)
        });
    }
    let mut answers = Vec::new();
    while let Some(joined) = asking.join_next().await {
        if let Ok(answer) = joined {
            answers.push(answer);
        }
    }
    answers.sort_by_key(|(index, _, _)| *index);
    answers
        .into_iter()
        .map(|(_, name, answer)| (name, answer))
        .collect()
}

/// One host's agents: `--all-sessions` where the build has it, and one session at a time where it
/// does not.
///
/// The fleet is upgraded one machine at a time, so a host can be a release behind the one asking.
/// A build from before `--all-sessions` refuses the flag as a usage error, exit 2. That host is
/// asked for its sessions with `ls --json` and then for each session's agents, and the answer is
/// put in the same shape, each row carrying its `session`.
async fn ask(target: &Target, argv: &[String]) -> Answer {
    let outcome = invoke::run_on(target, argv).await?;
    if outcome.code != 2 || !outcome.stderr.contains("--all-sessions") {
        return Ok(outcome);
    }
    let sessions = invoke::run_on(target, &["ls".to_owned(), "--json".to_owned()]).await?;
    if sessions.is_error() {
        return Ok(sessions);
    }
    let names: Vec<String> = serde_json::from_str::<Vec<Value>>(sessions.stdout.trim())
        .unwrap_or_default()
        .iter()
        .filter(|session| session.get("is_dead").and_then(Value::as_bool) != Some(true))
        .filter_map(|session| session.get("name").and_then(Value::as_str))
        .map(str::to_owned)
        .collect();
    let mut agents = Vec::new();
    for name in names {
        let one: Vec<String> = ["-s", name.as_str(), "action", "list-agents", "--json"]
            .iter()
            .map(|word| (*word).to_owned())
            .collect();
        // a session that does not answer is left out, as `--all-sessions` leaves it out
        let Ok(said) = invoke::run_on(target, &one).await else {
            continue;
        };
        let Ok(Value::Array(rows)) = serde_json::from_str::<Value>(said.stdout.trim()) else {
            continue;
        };
        for mut row in rows {
            if let Some(row) = row.as_object_mut() {
                row.insert("session".to_owned(), json!(name));
            }
            agents.push(row);
        }
    }
    Ok(Outcome {
        code: 0,
        stdout: json!(agents).to_string(),
        stderr: String::new(),
    })
}

/// The answers as one result: [`HOW_TO_MESSAGE`] and the [`grouped`] view for a reader, and the
/// flat rows, each with its `host` (and `host_alias`, when the list gives it one), for a program.
/// A host that did not answer is a host entry with its `error`.
pub fn merge(
    answers: &[(String, Answer)],
    now_ms: u64,
    listed: &RemoteHosts,
    local_host: Option<&str>,
) -> CallToolResult {
    let warnings = &listed.warnings;
    let view = AgentsView { only_tab: None };
    let mut agents = Vec::new();
    let mut hosts = Vec::new();
    for (host, answer) in answers {
        // `unreachable` is ssh's own failure, or no answer in time. `error` is a host that
        // answered and said no
        let said = match answer {
            Ok(outcome) if !outcome.is_error() => Ok(outcome),
            Ok(outcome) if outcome.code == 255 => {
                Err(("unreachable", first_line(&outcome.stderr, &outcome.stdout)))
            },
            Ok(outcome) => Err(("error", first_line(&outcome.stderr, &outcome.stdout))),
            Err(message) => Err(("unreachable", first_line(message, ""))),
        };
        let alias = listed.alias_of(host);
        let with_alias = |mut entry: Value| {
            if let (Some(alias), Some(entry)) = (alias, entry.as_object_mut()) {
                entry.insert("host_alias".to_owned(), json!(alias));
            }
            entry
        };
        let outcome = match said {
            Ok(outcome) => outcome,
            Err((label, why)) => {
                hosts.push(with_alias(
                    json!({"host": host, "error": why, "error_kind": label}),
                ));
                continue;
            },
        };
        let shaped = view
            .shape(&outcome.stdout, now_ms)
            .and_then(|shaped| serde_json::from_str::<Value>(&shaped).ok());
        let rows = match shaped {
            Some(Value::Array(rows)) => rows,
            _ => {
                let why = "its answer was not the JSON list-agents prints".to_owned();
                hosts.push(with_alias(
                    json!({"host": host, "error": why, "error_kind": "error"}),
                ));
                continue;
            },
        };
        hosts.push(with_alias(json!({"host": host, "agents": rows.len()})));
        for mut row in rows {
            if let Some(row) = row.as_object_mut() {
                row.insert("host".to_owned(), json!(host));
                if let Some(alias) = alias {
                    row.insert("host_alias".to_owned(), json!(alias));
                }
            }
            agents.push(row);
        }
    }
    let mut structured = Map::new();
    structured.insert("exit_code".to_owned(), json!(0));
    structured.insert(
        "command".to_owned(),
        json!(format!("zellij {}", list_agents_argv().join(" "))),
    );
    structured.insert(
        "result".to_owned(),
        json!({"agents": agents, "hosts": hosts}),
    );
    if !warnings.is_empty() {
        structured.insert("diagnostics".to_owned(), json!(warnings.join("\n")));
    }
    let mut result = CallToolResult::success(Vec::new());
    result.structured_content = Some(Value::Object(structured));
    let view = grouped(&hosts, &agents, now_ms, local_host, None);
    with_grouped(&mut result, view);
    result
}

/// The one instruction an agent listing carries: how to send a message to a row.
pub const HOW_TO_MESSAGE: &str = "To message an agent, call zellij_send with to = \
host/tab, host/handle or the agent's name: \"X on <machine>\" is to=<machine>/X, and one agent \
must match. It waits until that agent is idle and nobody is typing there, and says delivered, \
queued or refused. A pane entry's address works as to = host/session/handle. message_to, when \
present, is a second route: the harness's own messaging tool.";

/// The agent rows as a person refers to them: host, then tab, then pane.
///
/// `hosts` are the host entries in order (`{host, host_alias?, error?}`); a row's `host` places it,
/// and a row whose host is not among them adds one. Every key of a row stays in its pane entry,
/// which gains `display_title` (the title, or "new session" when the harness has given none),
/// `chat_age_s` and `address`, the `session` and `pane` that `zellij_write_input` takes. A row of
/// `local_host` is addressed by its bare session, any other as `host/session`. `session` is the
/// session a row without its own `session` came from, when the call knows it.
pub fn grouped(
    hosts: &[Value],
    rows: &[Value],
    now_ms: u64,
    local_host: Option<&str>,
    session: Option<&str>,
) -> Value {
    let mut out: Vec<Map<String, Value>> = hosts
        .iter()
        .filter_map(|entry| entry.as_object())
        .map(|entry| {
            let mut host = entry.clone();
            host.remove("agents");
            host.insert("tabs".to_owned(), json!([]));
            host
        })
        .collect();
    for row in rows {
        let Some(fields) = row.as_object() else {
            continue;
        };
        let host_name = fields.get("host").and_then(Value::as_str).unwrap_or("");
        let at = match out
            .iter()
            .position(|host| host.get("host").and_then(Value::as_str) == Some(host_name))
        {
            Some(at) => at,
            None => {
                let mut host = Map::new();
                host.insert("host".to_owned(), json!(host_name));
                if let Some(alias) = fields.get("host_alias") {
                    host.insert("host_alias".to_owned(), alias.clone());
                }
                host.insert("tabs".to_owned(), json!([]));
                out.push(host);
                out.len() - 1
            },
        };
        let row_session = fields
            .get("session")
            .and_then(Value::as_str)
            .or(session)
            .map(str::to_owned);
        let tab_name = fields.get("tab_name").cloned().unwrap_or(Value::Null);
        let mut pane = fields.clone();
        pane.insert(
            "display_title".to_owned(),
            json!(display_title(
                fields.get("title").and_then(Value::as_str).unwrap_or(""),
                fields.get("kind").and_then(Value::as_str).unwrap_or(""),
            )),
        );
        if let Some(started) = fields.get("chat_started_at").and_then(Value::as_u64) {
            pane.insert(
                "chat_age_s".to_owned(),
                json!(now_ms.saturating_sub(started) / 1000),
            );
        }
        let local = !host_name.is_empty() && local_host == Some(host_name);
        let address_session = match (&row_session, local || host_name.is_empty()) {
            (Some(session), true) => Some(session.clone()),
            (Some(session), false) => Some(format!("{}/{}", host_name, session)),
            (None, true) => None,
            (None, false) => Some(format!("{}/", host_name)),
        };
        let mut address = Map::new();
        if let Some(address_session) = address_session {
            address.insert("session".to_owned(), json!(address_session));
        }
        address.insert(
            "pane".to_owned(),
            fields.get("handle").cloned().unwrap_or(Value::Null),
        );
        pane.insert("address".to_owned(), Value::Object(address));
        let Some(Value::Array(tabs)) = out[at].get_mut("tabs") else {
            continue;
        };
        let tab_at = tabs.iter().position(|tab| {
            tab.get("tab") == Some(&tab_name)
                && tab.get("session").and_then(Value::as_str) == row_session.as_deref()
        });
        let tab_at = match tab_at {
            Some(tab_at) => tab_at,
            None => {
                let mut tab = Map::new();
                if let Some(session) = &row_session {
                    tab.insert("session".to_owned(), json!(session));
                }
                tab.insert("tab".to_owned(), tab_name.clone());
                tab.insert("panes".to_owned(), json!([]));
                tabs.push(Value::Object(tab));
                tabs.len() - 1
            },
        };
        if let Some(Value::Array(panes)) = tabs[tab_at].get_mut("panes") {
            panes.push(Value::Object(pane));
        }
    }
    Value::Array(out.into_iter().map(Value::Object).collect())
}

/// A pane title, or "new session" when it is empty or only the harness's own name: a claude that
/// has not been given a task is titled "Claude Code", after a spinner glyph.
pub fn display_title(title: &str, kind: &str) -> String {
    let bare = title
        .trim()
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .trim();
    let generic = bare.is_empty()
        || bare.eq_ignore_ascii_case(kind)
        || ["claude code", "claude", "codex", "opencode", "pi"]
            .iter()
            .any(|name| bare.eq_ignore_ascii_case(name));
    if generic {
        "new session".to_owned()
    } else {
        title.trim().to_owned()
    }
}

/// The text and structure an agent listing answers with: [`HOW_TO_MESSAGE`], then the grouped
/// view as JSON. The structure keeps its flat `result` and gains `grouped` and `how_to_message`.
pub fn with_grouped(result: &mut CallToolResult, grouped: Value) {
    let text = format!(
        "{}\n{}",
        HOW_TO_MESSAGE,
        serde_json::to_string_pretty(&grouped).unwrap_or_default()
    );
    result.content = vec![ContentBlock::text(text)];
    if let Some(Value::Object(structured)) = result.structured_content.as_mut() {
        structured.insert("grouped".to_owned(), grouped);
        structured.insert("how_to_message".to_owned(), json!(HOW_TO_MESSAGE));
    }
}

fn first_line(first: &str, then: &str) -> String {
    let said = if first.trim().is_empty() { then } else { first };
    said.trim()
        .lines()
        .next()
        .filter(|line| !line.is_empty())
        .unwrap_or("it said nothing")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(stdout: &str) -> Answer {
        Ok(Outcome {
            code: 0,
            stdout: stdout.to_owned(),
            stderr: String::new(),
        })
    }

    const BOXA: &str = r#"[
        {"handle": "brisk-heron", "name": "project-1", "self": false, "kind": "claude", "session": "work", "tab_name": "build",
         "state": "blocked", "state_kind": "permission", "cwd": "/srv/app",
         "last_output_at": 1000000, "command": "claude"}
    ]"#;

    #[test]
    fn rows_carry_their_host_and_a_silent_host_is_one_row() {
        let answers = vec![
            ("boxa".to_owned(), ok(BOXA)),
            (
                "boxb".to_owned(),
                Ok(Outcome {
                    code: 255,
                    stdout: String::new(),
                    stderr: "ssh could not reach boxb (exit 255): Connection timed out\nmore"
                        .to_owned(),
                }),
            ),
            ("boxc".to_owned(), ok("[]")),
            (
                "boxd".to_owned(),
                Err("no answer within 10 seconds".to_owned()),
            ),
        ];
        let listed = RemoteHosts {
            hosts: vec![remote::RemoteHost {
                name: "boxa".into(),
                bin: None,
                alias: Some("a".into()),
            }],
            warnings: vec!["a warning".to_owned()],
            path: None,
        };
        let result = merge(&answers, 1_042_000, &listed, Some("boxz"));
        assert_eq!(result.is_error, Some(false));
        let text = serde_json::to_string(&result.content).unwrap();
        let text = serde_json::from_str::<Value>(&text).unwrap()[0]["text"]
            .as_str()
            .unwrap()
            .to_owned();
        let (first, rest) = text.split_once('\n').unwrap();
        assert_eq!(first, HOW_TO_MESSAGE);
        let view: Value = serde_json::from_str(rest).unwrap();
        let pane = &view[0]["tabs"][0]["panes"][0];
        assert_eq!(view[0]["host"], "boxa");
        assert_eq!(view[0]["host_alias"], "a");
        assert_eq!(view[0]["tabs"][0]["tab"], "build");
        assert_eq!(view[0]["tabs"][0]["session"], "work");
        assert_eq!(pane["name"], "project-1");
        assert_eq!(pane["output_age_s"], 42);
        assert_eq!(
            pane["address"],
            json!({"session": "boxa/work", "pane": "brisk-heron"})
        );
        assert_eq!(
            view[1],
            json!({"host": "boxb", "error_kind": "unreachable", "tabs": [],
                   "error": "ssh could not reach boxb (exit 255): Connection timed out"})
        );
        assert_eq!(view[2], json!({"host": "boxc", "tabs": []}));
        assert_eq!(view[3]["error"], "no answer within 10 seconds");
        let structured = result.structured_content.unwrap();
        assert_eq!(structured["grouped"], view);
        assert_eq!(structured["how_to_message"], HOW_TO_MESSAGE);
        let agents = structured["result"]["agents"].as_array().unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0]["host"], json!("boxa"));
        assert_eq!(agents[0]["host_alias"], json!("a"));
        assert_eq!(agents[0]["name"], "project-1");
        assert_eq!(agents[0]["self"], false);
        assert_eq!(agents[0]["output_age_s"], json!(42));
        let hosts = &structured["result"]["hosts"];
        assert_eq!(
            hosts[0],
            json!({"host": "boxa", "host_alias": "a", "agents": 1})
        );
        assert_eq!(
            hosts[1]["error"],
            "ssh could not reach boxb (exit 255): Connection timed out"
        );
        assert_eq!(hosts[2], json!({"host": "boxc", "agents": 0}));
        assert_eq!(structured["diagnostics"], json!("a warning"));
    }

    #[test]
    fn the_grouped_view_is_host_then_tab_then_pane_and_keeps_every_key() {
        let rows: Vec<Value> = serde_json::from_str(
            r#"[
            {"host": "here", "session": "work", "tab_name": "develop", "handle": "deep-fox",
             "kind": "claude", "name": "develop-1", "title": "✳ Claude Code", "state": "idle",
             "chat_started_at": 1000000, "context_tokens": 5000,
             "message_to": "bridge:session_01A", "message_route": "harness"},
            {"host": "boxa", "host_alias": "a", "session": "work", "tab_name": "develop",
             "handle": "calm-otter", "kind": "codex", "title": "Fix the build",
             "message_route": "pane"},
            {"host": "here", "session": "work", "tab_name": "develop", "handle": "warm-owl",
             "kind": "pi", "title": "", "message_route": "pane"},
            {"host": "here", "session": "play", "tab_name": "develop", "handle": "odd-elk",
             "kind": "opencode", "title": "opencode"}
        ]"#,
        )
        .unwrap();
        let hosts = vec![json!({"host": "here", "agents": 3})];
        let view = grouped(&hosts, &rows, 61_000_000, Some("here"), None);
        assert_eq!(view.as_array().map(Vec::len), Some(2));
        let here = &view[0];
        assert!(here.get("agents").is_none(), "the count is not a pane key");
        // one tab per session and name, in the order rows arrived
        let tabs: Vec<(&str, &str)> = here["tabs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tab| {
                (
                    tab["session"].as_str().unwrap(),
                    tab["tab"].as_str().unwrap(),
                )
            })
            .collect();
        assert_eq!(tabs, vec![("work", "develop"), ("play", "develop")]);
        let first = &here["tabs"][0]["panes"][0];
        for key in [
            "host",
            "session",
            "tab_name",
            "kind",
            "name",
            "state",
            "message_to",
        ] {
            assert_eq!(first[key], rows[0][key], "{}", key);
        }
        assert_eq!(first["display_title"], "new session");
        assert_eq!(first["chat_age_s"], 60_000);
        // this machine's rows are addressed by their bare session
        assert_eq!(
            first["address"],
            json!({"session": "work", "pane": "deep-fox"})
        );
        assert_eq!(here["tabs"][0]["panes"][1]["display_title"], "new session");
        assert_eq!(here["tabs"][1]["panes"][0]["display_title"], "new session");
        // a host the entries did not name is added, with its alias, and addressed host/session
        assert_eq!(view[1]["host"], "boxa");
        assert_eq!(view[1]["host_alias"], "a");
        let remote = &view[1]["tabs"][0]["panes"][0];
        assert_eq!(remote["display_title"], "Fix the build");
        assert_eq!(remote["address"]["session"], "boxa/work");
        assert!(remote.get("chat_age_s").is_none());
        // a row with no session of its own takes the call's, and a remote one with none names
        // the host alone
        let bare = vec![json!({"host": "boxa", "tab_name": "t", "handle": "h", "kind": "pi"})];
        let view = grouped(&[], &bare, 0, None, Some("work"));
        assert_eq!(
            view[0]["tabs"][0]["panes"][0]["address"]["session"],
            "boxa/work"
        );
        let view = grouped(&[], &bare, 0, None, None);
        assert_eq!(
            view[0]["tabs"][0]["panes"][0]["address"]["session"],
            "boxa/"
        );
    }

    /// The whole remote path with a stand-in ssh: it runs the remote line through `sh` as the
    /// remote login shell would, for one host, and fails like an unreachable host for the other.
    #[tokio::test]
    async fn one_host_failing_leaves_the_others_answer_whole() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let zellij = dir.path().join("zellij");
        std::fs::write(
            &zellij,
            format!(
                "#!/bin/sh\n[ \"$*\" = \"{}\" ] || exit 9\nprintf '%s' '{}'\n",
                list_agents_argv().join(" "),
                BOXA.replace('\n', " ")
            ),
        )
        .unwrap();
        let ssh = dir.path().join("ssh");
        std::fs::write(
            &ssh,
            "#!/bin/sh\n\
             for last in \"$@\"; do :; done\n\
             case \" $* \" in *' boxdown '*) echo 'ssh: connect to host boxdown: refused' >&2; exit 255;; esac\n\
             exec sh -c \"$last\"\n",
        )
        .unwrap();
        for file in [&zellij, &ssh] {
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // one test sets the variable, so no other test can see it change under it
        let _guard = EnvGuard::set(remote::SSH_PROGRAM_ENV, &ssh);
        let up = remote::RemoteHost {
            name: "boxa".into(),
            bin: Some(zellij.display().to_string()),
            alias: None,
        };
        let down = remote::RemoteHost {
            name: "boxdown".into(),
            bin: None,
            alias: None,
        };
        let answers = gather(
            vec![
                ("boxa".into(), Target::Remote(up)),
                ("boxdown".into(), Target::Remote(down)),
            ],
            Duration::from_secs(20),
        )
        .await;
        let result = merge(&answers, 1_042_000, &RemoteHosts::default(), None);
        let structured = result.structured_content.unwrap();
        let hosts = &structured["result"]["hosts"];
        assert_eq!(hosts[0]["agents"], json!(1), "{}", structured);
        assert!(
            hosts[1]["error"]
                .as_str()
                .unwrap()
                .contains("ssh could not reach boxdown (exit 255)"),
            "{}",
            structured
        );
        assert_eq!(structured["result"]["agents"][0]["host"], json!("boxa"));

        // a host a release behind, from before `--all-sessions`, is asked one session at a time
        let old = dir.path().join("old");
        std::fs::write(
            &old,
            "#!/bin/sh\n\
             case \"$*\" in\n\
             *--all-sessions*) echo \"error: unexpected argument '--all-sessions' found\" >&2; exit 2;;\n\
             'ls --json') echo '[{\"name\": \"work\"}, {\"name\": \"gone\", \"is_dead\": true}]';;\n\
             '-s work action list-agents --json') \
             echo '[{\"handle\": \"calm-otter\", \"kind\": \"codex\"}]';;\n\
             *) exit 9;;\n\
             esac\n",
        )
        .unwrap();
        std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o755)).unwrap();
        let behind = remote::RemoteHost {
            name: "boxold".into(),
            bin: Some(old.display().to_string()),
            alias: None,
        };
        let answers = gather(
            vec![("boxold".into(), Target::Remote(behind))],
            Duration::from_secs(20),
        )
        .await;
        let result = merge(&answers, 0, &RemoteHosts::default(), None);
        let structured = result.structured_content.unwrap();
        let agents = &structured["result"]["agents"];
        assert_eq!(agents.as_array().map(Vec::len), Some(1), "{}", structured);
        assert_eq!(agents[0]["session"], json!("work"));
        assert_eq!(agents[0]["host"], json!("boxold"));

        // and a host that answers nothing is cut off at the bound, not waited on
        let slow = dir.path().join("slow");
        std::fs::write(&slow, "#!/bin/sh\nexec sleep 30\n").unwrap();
        std::fs::set_permissions(&slow, std::fs::Permissions::from_mode(0o755)).unwrap();
        let host = remote::RemoteHost {
            name: "boxslow".into(),
            bin: Some(slow.display().to_string()),
            alias: None,
        };
        let started = std::time::Instant::now();
        let answers = gather(
            vec![("boxslow".into(), Target::Remote(host))],
            Duration::from_secs(1),
        )
        .await;
        assert!(started.elapsed() < Duration::from_secs(10));
        assert_eq!(
            answers[0].1.as_ref().err().map(String::as_str),
            Some("no answer within 1 seconds")
        );
    }

    struct EnvGuard(&'static str);
    impl EnvGuard {
        fn set(name: &'static str, value: &std::path::Path) -> Self {
            std::env::set_var(name, value);
            EnvGuard(name)
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            std::env::remove_var(self.0);
        }
    }
}
