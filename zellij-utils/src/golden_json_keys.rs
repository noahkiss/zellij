//! Golden JSON key sets, captured from the released `0.45.1-nkmk.40` build.
//!
//! `fixtures/nkmk40/README.md` says what the fixtures are and how they were made. The key set of a
//! JSON document is every key path in it with the JSON types seen there. A program that reads this
//! build's JSON relies on those paths and types, so a later build must keep them.
//!
//! Run with `ZELLIJ_UPDATE_GOLDEN_KEYS=1` to rewrite the two checked-in key files.

use crate::data::{AgentListEntry, PaneListEntry};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

const FIXTURES: &[(&str, &str)] = &[
    (
        "list-tree.json",
        include_str!("fixtures/nkmk40/list-tree.json"),
    ),
    (
        "list-panes.json",
        include_str!("fixtures/nkmk40/list-panes.json"),
    ),
    (
        "list-agents.json",
        include_str!("fixtures/nkmk40/list-agents.json"),
    ),
    ("watch.jsonl", include_str!("fixtures/nkmk40/watch.jsonl")),
];

const EXPECTED_KEYS: &str = include_str!("fixtures/nkmk40/expected-keys.txt");
const NOT_IN_THIS_BUILD: &str = include_str!("fixtures/nkmk40/not-in-this-build.txt");

/// Key paths mapped to the JSON types seen at each one.
pub type KeySet = BTreeMap<String, BTreeSet<&'static str>>;

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn walk(value: &Value, path: String, keys: &mut KeySet) {
    keys.entry(path.clone())
        .or_default()
        .insert(type_name(value));
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                walk(child, format!("{}.{}", path, key), keys);
            }
        },
        // every element under one `[]` path: the union of their keys and types
        Value::Array(items) => {
            for item in items {
                walk(item, format!("{}[]", path), keys);
            }
        },
        _ => {},
    }
}

/// The key set of one JSON document, rooted at `$`.
pub fn key_set(json: &str) -> KeySet {
    let value: Value = serde_json::from_str(json).expect("the fixture is JSON");
    let mut keys = KeySet::new();
    walk(&value, "$".to_owned(), &mut keys);
    keys
}

/// The key set of an `action watch --json` stream, one root per `event` name.
pub fn jsonl_key_set(jsonl: &str) -> KeySet {
    let mut keys = KeySet::new();
    for line in jsonl.lines().filter(|line| !line.trim().is_empty()) {
        let value: Value = serde_json::from_str(line).expect("each line is JSON");
        let event = value
            .get("event")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_owned();
        walk(&value, event, &mut keys);
    }
    keys
}

fn fixture_key_set(name: &str, text: &str) -> KeySet {
    if name.ends_with(".jsonl") {
        jsonl_key_set(text)
    } else {
        key_set(text)
    }
}

fn render(sections: &[(&str, KeySet)]) -> String {
    let mut out = String::new();
    for (name, keys) in sections {
        out.push_str(&format!("# {}\n", name));
        for (path, types) in keys {
            let types: Vec<&str> = types.iter().copied().collect();
            out.push_str(&format!("{} {}\n", path, types.join("|")));
        }
        out.push('\n');
    }
    out
}

fn check_or_update(file: &str, expected: &str, rendered: String) {
    if std::env::var_os("ZELLIJ_UPDATE_GOLDEN_KEYS").is_some() {
        let path = format!(
            "{}/src/fixtures/nkmk40/{}",
            env!("CARGO_MANIFEST_DIR"),
            file
        );
        std::fs::write(&path, rendered).expect("write the key file");
        return;
    }
    assert!(
        rendered == expected,
        "the key set differs from fixtures/nkmk40/{}; rerun with ZELLIJ_UPDATE_GOLDEN_KEYS=1 \
         and review the diff\n{}",
        file,
        rendered
    );
}

#[test]
fn the_nkmk40_fixtures_have_the_recorded_key_sets() {
    let sections: Vec<(&str, KeySet)> = FIXTURES
        .iter()
        .map(|(name, text)| (*name, fixture_key_set(name, text)))
        .collect();
    check_or_update("expected-keys.txt", EXPECTED_KEYS, render(&sections));
}

/// Keys the CLI client adds to a `list-panes --json` row after the server wrote it, so no row
/// type carries them (`zellij-client/src/cli_client.rs`).
const CLIENT_ADDED: &[&str] = &["$[].self"];

/// The fixture's key set, minus the keys a parse into this build's type and back reproduces.
fn not_reproduced<T: DeserializeOwned + Serialize>(text: &str) -> KeySet {
    let rows: Vec<T> = serde_json::from_str(text).expect("this build reads the nkmk.40 rows");
    let again = serde_json::to_string(&rows).expect("this build writes the rows");
    let ours = key_set(&again);
    let mut missing = KeySet::new();
    for (path, types) in key_set(text) {
        if CLIENT_ADDED.contains(&path.as_str()) {
            continue;
        }
        let lost: BTreeSet<&'static str> = match ours.get(&path) {
            Some(ours) => types.difference(ours).copied().collect(),
            None => types,
        };
        if !lost.is_empty() {
            missing.insert(path, lost);
        }
    }
    missing
}

/// `list-panes` and `list-agents` rows read into this build's types and written back. What does
/// not survive is recorded: today, the keys the fork removed after nkmk.40. A change that loses a
/// key a reader relies on shows up as a new line in `not-in-this-build.txt`.
///
/// `list-tree` and `action watch` have no row type to read into: the server builds their JSON
/// as it writes it, so only their recorded key sets cover them.
#[test]
fn this_build_writes_the_nkmk40_list_keys() {
    let sections = vec![
        (
            "list-panes.json",
            not_reproduced::<PaneListEntry>(include_str!("fixtures/nkmk40/list-panes.json")),
        ),
        (
            "list-agents.json",
            not_reproduced::<AgentListEntry>(include_str!("fixtures/nkmk40/list-agents.json")),
        ),
    ];
    check_or_update(
        "not-in-this-build.txt",
        NOT_IN_THIS_BUILD,
        render(&sections),
    );
}

/// Upstream's `PaneInfo` carries a `command_state` struct of its own, and a `list-panes` row
/// flattens `PaneInfo`. The row's `command_state` is the fork's string, so upstream's struct is
/// written under another key: one `command_state`, a string or absent, never an object.
#[test]
fn a_list_panes_row_writes_one_command_state_and_it_is_a_string() {
    use crate::data::{PaneCommandState, PaneInfo};
    for command_state in [None, Some(PaneCommandState::Done)] {
        let row = PaneListEntry {
            pane_info: PaneInfo::default(),
            tab_position: 0,
            tab_name: String::new(),
            agent: None,
            command_state,
            last_command_exit_code: None,
            input_since_prompt: false,
            input_since_state: false,
            prefilled_command: None,
            queued_messages: 0,
            program_status: None,
        };
        let raw = serde_json::to_string(&row).expect("the row serializes");
        let count = raw.matches("\"command_state\":").count();
        match command_state {
            Some(_) => {
                assert_eq!(count, 1, "{}", raw);
                assert!(raw.contains("\"command_state\":\"done\""), "{}", raw);
            },
            None => assert_eq!(count, 0, "{}", raw),
        }
        assert!(raw.contains("\"command_state_detail\":{"), "{}", raw);
    }
}
