//! `zellij_send`: a message to a coding agent in another pane, on this machine or another.
//!
//! The tool runs `zellij action send-message` where the receiver lives - over ssh for another
//! machine - so the session that holds a message is the receiver's own, and a held message waits
//! there for the agent to be idle. This file says where a call goes and who it is from; the
//! delivery rule is `zellij_utils::agent_mail`.

use serde_json::{json, Map, Value};

use super::remote::{self, RemoteHosts, Target};

/// Where a send goes: the machine, the session on it if one was named, and the target the
/// receiving CLI resolves.
#[derive(Debug, PartialEq, Eq)]
pub struct Route {
    pub target: Target,
    pub session: Option<String>,
    pub to: String,
}

/// Reads `to` and `session` into a route.
///
/// `session` takes `host/session` like every tool. `to` may itself start with a host or its alias:
/// `boxa/develop` is the agent `develop` names on `boxa`, in whichever session it runs, and
/// `boxa/work/develop` narrows that to session `work`. A first word that names no host is left to
/// the receiving CLI, which reads `work/develop` as session and target.
pub fn route(
    to: &str,
    session: Option<&str>,
    hosts: &RemoteHosts,
    local: &str,
) -> Result<Route, String> {
    let mut arguments = Map::new();
    if let Some(session) = session {
        arguments.insert("session".to_owned(), json!(session));
    }
    let session_named_host = session.is_some_and(|s| s.contains(remote::HOST_SEPARATOR));
    let mut target = remote::route(&mut arguments, hosts, local)?;
    let mut session = arguments
        .get("session")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let mut to = to.to_owned();
    if !session_named_host {
        if let Some((first, rest)) = to.split_once('/') {
            let listed = hosts.find(first);
            let this_machine = remote::is_local(first, local)
                || listed.is_some_and(|host| remote::is_local(&host.name, local));
            if this_machine || listed.is_some() {
                target = if this_machine {
                    Target::Local
                } else {
                    Target::Remote(listed.cloned().unwrap_or_else(|| unreachable!()))
                };
                to = rest.to_owned();
            }
        }
    }
    if to.is_empty() {
        return Err(
            "`to` names no agent: give a handle, a tab, an agent name or a project.".into(),
        );
    }
    // `session/target` in `to` and a `session` argument would be two answers to one question
    if session.is_some() && to.contains('/') {
        session = None;
    }
    Ok(Route {
        target,
        session,
        to,
    })
}

/// Who a message is from: this machine, this server's session, and the pane it runs in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sender {
    pub address: String,
    pub name: Option<String>,
}

/// The sender, from this server's own pane row in `list-panes --json`.
pub fn sender(host: &str, session: Option<&str>, pane_id: Option<u32>, panes: &str) -> Sender {
    let session = session.unwrap_or("-");
    let row = pane_id.and_then(|id| {
        serde_json::from_str::<Value>(panes)
            .ok()?
            .as_array()?
            .iter()
            .find(|row| {
                row.get("id").and_then(Value::as_u64) == Some(id as u64)
                    && row.get("is_plugin").and_then(Value::as_bool) != Some(true)
            })
            .cloned()
    });
    let handle = row
        .as_ref()
        .and_then(|row| row.get("handle"))
        .and_then(Value::as_str)
        .filter(|handle| !handle.is_empty())
        .map(str::to_owned)
        .or_else(|| pane_id.map(|id| format!("terminal_{}", id)))
        .unwrap_or_else(|| "mcp".to_owned());
    let name = row
        .as_ref()
        .and_then(|row| row.get("agent"))
        .and_then(|agent| agent.get("name"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    Sender {
        address: format!("{}/{}/{}", host, session, handle),
        name,
    }
}

/// The command line the receiving machine runs.
pub fn argv(
    route: &Route,
    sender: &Sender,
    args: &Map<String, Value>,
) -> Result<Vec<String>, String> {
    let text = args
        .get("text")
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .ok_or("`text` is required and was not given.")?;
    let mut argv = Vec::new();
    if let Some(session) = &route.session {
        argv.extend(["-s".to_owned(), session.clone()]);
    }
    argv.extend([
        "action".to_owned(),
        "send-message".to_owned(),
        "--to".to_owned(),
        route.to.clone(),
    ]);
    // no session named: the target is looked for in every session on that machine
    if route.session.is_none() {
        argv.push("--all-sessions".to_owned());
    }
    if let Some(expire_s) = number(args, "expire_s") {
        argv.extend(["--expire-s".to_owned(), expire_s.to_string()]);
    }
    if let Some(wait_s) = number(args, "wait_s") {
        argv.extend(["--wait".to_owned(), wait_s.to_string()]);
    }
    argv.extend(["--from".to_owned(), sender.address.clone()]);
    if let Some(name) = &sender.name {
        argv.extend(["--from-name".to_owned(), name.clone()]);
    }
    if let Some(reply_to) = args.get("reply_to").and_then(Value::as_str) {
        argv.extend(["--reply-to".to_owned(), reply_to.to_owned()]);
    }
    argv.extend(["--json".to_owned(), "--".to_owned(), text.to_owned()]);
    Ok(argv)
}

fn number(args: &Map<String, Value>, name: &str) -> Option<u64> {
    match args.get(name)? {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

/// Whether the receiving machine's zellij is a build without `send-message`.
///
/// clap names the word it did not know. Such a build gets no fallback to typing the message in:
/// an unguarded paste into a person's half-written prompt is the very failure this tool exists to
/// prevent, so the call is refused and says which machine needs the newer build.
pub fn verb_unknown(stderr: &str) -> bool {
    stderr.contains("send-message")
        && (stderr.contains("unrecognized subcommand")
            || stderr.contains("unexpected argument")
            || stderr.contains("Found argument")
            || stderr.contains("wasn't expected")
            || stderr.contains("invalid subcommand"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hosts() -> RemoteHosts {
        RemoteHosts::parse(
            "remote_hosts {\n  host \"boxa.example.net\" alias=\"a\"\n  host \"boxb\"\n}\n",
        )
    }

    fn remote(name: &str) -> Target {
        Target::Remote(hosts().find(name).unwrap().clone())
    }

    #[test]
    fn a_host_or_its_alias_at_the_front_of_to_picks_the_machine() {
        let hosts = hosts();
        let r = route("a/develop", None, &hosts, "here").unwrap();
        assert_eq!(r.target, remote("a"));
        assert_eq!((r.session, r.to.as_str()), (None, "develop"));
        let r = route("boxb/work/develop", None, &hosts, "here").unwrap();
        assert_eq!(r.target, remote("boxb"));
        assert_eq!(r.to, "work/develop");
        // this machine by name stays local
        let r = route("here/develop", None, &hosts, "here").unwrap();
        assert_eq!((r.target, r.to.as_str()), (Target::Local, "develop"));
        // a first word that is no host is the receiving CLI's session/target
        let r = route("work/develop", None, &hosts, "here").unwrap();
        assert_eq!((r.target, r.to.as_str()), (Target::Local, "work/develop"));
    }

    #[test]
    fn the_session_argument_routes_as_it_does_for_every_tool() {
        let hosts = hosts();
        let r = route("develop", Some("a/work"), &hosts, "here").unwrap();
        assert_eq!(r.target, remote("a"));
        assert_eq!(r.session.as_deref(), Some("work"));
        let r = route("develop", Some("work"), &hosts, "here").unwrap();
        assert_eq!(
            (r.target, r.session.as_deref()),
            (Target::Local, Some("work"))
        );
        assert!(route("develop", Some("nowhere/work"), &hosts, "here").is_err());
        assert!(route("a/", None, &hosts, "here").is_err());
    }

    #[test]
    fn the_command_line_carries_the_sender_and_searches_every_session_when_none_is_named() {
        let r = route("a/develop", None, &hosts(), "here").unwrap();
        let sender = Sender {
            address: "here/main/brisk-heron".into(),
            name: Some("builder".into()),
        };
        let args: Map<String, Value> = serde_json::from_value(json!({
            "to": "a/develop", "text": "--looks like a flag", "expire_s": 600, "wait_s": "20",
            "reply_to": "m1a2b3c"
        }))
        .unwrap();
        assert_eq!(
            argv(&r, &sender, &args).unwrap().join(" "),
            "action send-message --to develop --all-sessions --expire-s 600 --wait 20 \
             --from here/main/brisk-heron --from-name builder --reply-to m1a2b3c --json -- \
             --looks like a flag"
        );
        let r = route("develop", Some("work"), &hosts(), "here").unwrap();
        let args: Map<String, Value> = serde_json::from_value(json!({"text": "hi"})).unwrap();
        assert_eq!(
            argv(&r, &sender, &args).unwrap()[..5].join(" "),
            "-s work action send-message --to"
        );
    }

    #[test]
    fn the_sender_is_this_servers_own_pane() {
        let panes = r#"[{"id":3,"is_plugin":true,"handle":"plug"},
                        {"id":3,"is_plugin":false,"handle":"brisk-heron","agent":{"kind":"claude","name":"builder"}}]"#;
        assert_eq!(
            sender("here", Some("main"), Some(3), panes),
            Sender {
                address: "here/main/brisk-heron".into(),
                name: Some("builder".into())
            }
        );
        assert_eq!(sender("here", None, None, "[]").address, "here/-/mcp");
        assert_eq!(
            sender("here", Some("main"), Some(9), "[]").address,
            "here/main/terminal_9"
        );
    }

    #[test]
    fn an_older_build_is_told_apart_from_a_miss() {
        assert!(verb_unknown(
            "error: unrecognized subcommand 'send-message'\n\nUsage: zellij action <COMMAND>"
        ));
        assert!(!verb_unknown("No agent answers to 'develop'. The agents:"));
    }
}
