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
    [
        "action",
        "list-agents",
        "--all-sessions",
        "--json",
        "--report-withheld",
    ]
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
    merge(&answers, now_ms, &hosts.warnings)
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
    let outcome = invoke::run_on(target, argv, None).await?;
    if outcome.code != 2 || !outcome.stderr.contains("--all-sessions") {
        return Ok(outcome);
    }
    let sessions = invoke::run_on(target, &["ls".to_owned(), "--json".to_owned()], None).await?;
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
    let mut withheld = 0;
    for name in names {
        let one: Vec<String> = [
            "-s",
            name.as_str(),
            "action",
            "list-agents",
            "--json",
            "--report-withheld",
        ]
        .iter()
        .map(|word| (*word).to_owned())
        .collect();
        // a session that does not answer is left out, as `--all-sessions` leaves it out
        let Ok(said) = invoke::run_on(target, &one, None).await else {
            continue;
        };
        let Ok(Value::Object(mut envelope)) = serde_json::from_str::<Value>(said.stdout.trim())
        else {
            continue;
        };
        withheld += envelope
            .get("withheld")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        if let Some(Value::Array(rows)) = envelope.remove("agents") {
            for mut row in rows {
                if let Some(row) = row.as_object_mut() {
                    row.insert("session".to_owned(), json!(name));
                }
                agents.push(row);
            }
        }
    }
    Ok(Outcome {
        code: 0,
        stdout: json!({"agents": agents, "withheld": withheld}).to_string(),
        stderr: String::new(),
    })
}

/// The answers as one result: a line per agent and per silent host for a reader, and the rows,
/// each with its `host`, for a program.
pub fn merge(answers: &[(String, Answer)], now_ms: u64, warnings: &[String]) -> CallToolResult {
    let view = AgentsView { only_tab: None };
    let mut agents = Vec::new();
    let mut hosts = Vec::new();
    let mut lines = Vec::new();
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
        let outcome = match said {
            Ok(outcome) => outcome,
            Err((label, why)) => {
                lines.push(format!("{}: {}: {}", host, label, why));
                hosts.push(json!({"host": host, "error": why}));
                continue;
            },
        };
        let shaped = view
            .shape(&outcome.stdout, now_ms)
            .and_then(|shaped| serde_json::from_str::<Value>(&shaped).ok());
        let (rows, withheld) = match shaped {
            Some(Value::Array(rows)) => (rows, 0),
            Some(Value::Object(mut envelope)) => (
                match envelope.remove("agents") {
                    Some(Value::Array(rows)) => rows,
                    _ => Vec::new(),
                },
                envelope
                    .get("withheld")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            ),
            _ => {
                let why = "its answer was not the JSON list-agents prints".to_owned();
                lines.push(format!("{}: error: {}", host, why));
                hosts.push(json!({"host": host, "error": why}));
                continue;
            },
        };
        if rows.is_empty() {
            lines.push(format!("{}: no agents", host));
        }
        let mut summary = json!({"host": host, "agents": rows.len()});
        if withheld > 0 {
            summary["withheld"] = json!(withheld);
        }
        hosts.push(summary);
        for mut row in rows {
            if let Some(row) = row.as_object_mut() {
                lines.push(row_line(host, row));
                row.insert("host".to_owned(), json!(host));
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
    let mut result = CallToolResult::success(vec![ContentBlock::text(lines.join("\n"))]);
    result.structured_content = Some(Value::Object(structured));
    result
}

/// One agent on one line, with its live messaging name before its pane handle.
fn row_line(host: &str, row: &Map<String, Value>) -> String {
    let text = |key: &str| row.get(key).and_then(Value::as_str).unwrap_or("-");
    let mut state = text("state").to_owned();
    if let Some(kind) = row.get("state_kind").and_then(Value::as_str) {
        state = format!("{}/{}", state, kind);
    }
    let name = if row.get("self").and_then(Value::as_bool) == Some(true) {
        format!("{} (you)", text("name"))
    } else {
        text("name").to_owned()
    };
    let mut line = format!(
        "{}: {}: {} {} {} {} tab={}",
        host,
        text("session"),
        name,
        text("handle"),
        text("kind"),
        state,
        text("tab_name"),
    );
    if let Some(age) = row.get("output_age_s").and_then(Value::as_u64) {
        line.push_str(&format!(" out={}s", age));
    }
    if let Some(cwd) = row.get("cwd").and_then(Value::as_str) {
        line.push(' ');
        line.push_str(cwd);
    }
    line
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

    #[test]
    fn fleet_text_marks_the_named_caller() {
        let row = json!({"session":"work","name":"project-1","self":true,"handle":"brisk-heron","kind":"claude"});
        assert!(row_line("boxa", row.as_object().unwrap())
            .contains("project-1 (you) brisk-heron claude"));
    }

    const BOXA: &str = r#"{"agents": [
        {"handle": "brisk-heron", "name": "project-1", "self": false, "kind": "claude", "session": "work", "tab_name": "build",
         "state": "blocked", "state_kind": "permission", "cwd": "/srv/app",
         "last_output_at": 1000000, "command": "claude"}
    ], "withheld": 1}"#;

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
        let result = merge(&answers, 1_042_000, &["a warning".to_owned()]);
        assert_eq!(result.is_error, Some(false));
        let text = serde_json::to_string(&result.content).unwrap();
        let text: Vec<String> = serde_json::from_str::<Value>(&text).unwrap()[0]["text"]
            .as_str()
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        assert_eq!(
            text,
            vec![
                "boxa: work: project-1 brisk-heron claude blocked/permission tab=build out=42s /srv/app",
                "boxb: unreachable: ssh could not reach boxb (exit 255): Connection timed out",
                "boxc: no agents",
                "boxd: unreachable: no answer within 10 seconds",
            ]
        );
        let structured = result.structured_content.unwrap();
        let agents = structured["result"]["agents"].as_array().unwrap();
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0]["host"], json!("boxa"));
        assert_eq!(agents[0]["name"], "project-1");
        assert_eq!(agents[0]["self"], false);
        assert_eq!(agents[0]["output_age_s"], json!(42));
        let hosts = &structured["result"]["hosts"];
        assert_eq!(
            hosts[0],
            json!({"host": "boxa", "agents": 1, "withheld": 1})
        );
        assert!(hosts[1]["error"].as_str().unwrap().contains("exit 255"));
        assert_eq!(hosts[2], json!({"host": "boxc", "agents": 0}));
        assert_eq!(structured["diagnostics"], json!("a warning"));
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
        };
        let down = remote::RemoteHost {
            name: "boxdown".into(),
            bin: None,
        };
        let answers = gather(
            vec![
                ("boxa".into(), Target::Remote(up)),
                ("boxdown".into(), Target::Remote(down)),
            ],
            Duration::from_secs(20),
        )
        .await;
        let result = merge(&answers, 1_042_000, &[]);
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
             '-s work action list-agents --json --report-withheld') \
             echo '{\"agents\": [{\"handle\": \"calm-otter\", \"kind\": \"codex\"}], \"withheld\": 0}';;\n\
             *) exit 9;;\n\
             esac\n",
        )
        .unwrap();
        std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o755)).unwrap();
        let behind = remote::RemoteHost {
            name: "boxold".into(),
            bin: Some(old.display().to_string()),
        };
        let answers = gather(
            vec![("boxold".into(), Target::Remote(behind))],
            Duration::from_secs(20),
        )
        .await;
        let result = merge(&answers, 0, &[]);
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
