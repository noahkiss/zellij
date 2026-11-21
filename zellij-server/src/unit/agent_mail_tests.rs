use super::*;
use zellij_utils::agent_mail::{MessageRecord, Reason, Status};

fn typed(keys: &[&[u8]]) -> Draft {
    typed_while(keys, false)
}

fn typed_while(keys: &[&[u8]], working: bool) -> Draft {
    let now = Instant::now();
    let mut draft = Draft::default();
    for key in keys {
        draft.note(key, working, now);
    }
    draft
}

#[test]
fn typing_makes_a_draft_and_deleting_it_all_makes_it_clean_again() {
    assert_eq!(typed(&[b"h", b"i"]), Draft::Typed(2));
    assert_eq!(typed(&[b"h", b"i", b"\x7f", b"\x7f"]), Draft::Clean);
    assert_eq!(typed(&[b"h", b"\x7f", b"\x7f", b"\x7f"]), Draft::Clean);
    // multi-byte characters count once each
    assert_eq!(typed(&["é✓".as_bytes()]), Draft::Typed(2));
    // the forward delete key, and the kitty protocol's backspace
    assert_eq!(typed(&[b"ab", b"\x1b[3~", b"\x1b[127u"]), Draft::Clean);
}

#[test]
fn ctrl_c_and_ctrl_u_clear_the_draft_in_either_keyboard_protocol() {
    assert_eq!(typed(&[b"hello", b"\x03"]), Draft::Clean);
    assert_eq!(typed(&[b"hello", b"\x15"]), Draft::Clean);
    assert_eq!(typed(&[b"hello", b"\x1b[99;5u"]), Draft::Clean);
    assert_eq!(typed(&[b"hello", b"\x1b[117;5u"]), Draft::Clean);
    // an unknown key is cleared too
    assert_eq!(typed(&[b"\x1b[A", b"\x03"]), Draft::Clean);
}

#[test]
fn a_paste_counts_its_characters() {
    assert_eq!(typed(&[b"\x1b[200~three\x1b[201~"]), Draft::Typed(5));
    assert_eq!(typed(&[b"\x1b[200~\x1b[201~"]), Draft::Clean);
}

#[test]
fn moving_the_cursor_changes_nothing_but_a_key_with_unknown_effect_holds() {
    assert_eq!(
        typed(&[b"ab", b"\x1b[D", b"\x1b[C", b"\x1bOD"]),
        Draft::Typed(2)
    );
    assert_eq!(typed(&[b"\x1b[D"]), Draft::Clean);
    // focus reports and mouse reports are not keys
    assert_eq!(
        typed(&[b"\x1b[I", b"\x1b[O", b"\x1b[<0;3;4M"]),
        Draft::Clean
    );
    // Up recalls a line from history; Tab completes; Ctrl r opens a search; Alt x is anything
    for key in [&b"\x1b[A"[..], b"\t", b"\x12", b"\x1bx", b"\x1bOA"] {
        assert_eq!(typed(&[key]), Draft::Unknown, "{key:?}");
    }
    // typing and deleting cannot bring an unknown draft back to clean
    assert_eq!(typed(&[b"\x1b[A", b"x", b"\x7f", b"\x7f"]), Draft::Unknown);
}

#[test]
fn esc_interrupts_a_working_agent_but_may_open_a_menu_on_an_idle_one() {
    assert_eq!(typed_while(&[b"\x1b"], true), Draft::Clean);
    assert_eq!(typed_while(&[b"\x1b[27u"], true), Draft::Clean);
    assert_eq!(typed_while(&[b"\x1b"], false), Draft::Unknown);
    assert_eq!(typed_while(&[b"\x1b", b"\x1b"], false), Draft::Unknown);
}

#[test]
fn enter_on_text_is_a_submit_that_stays_pending_until_the_agent_takes_it_or_the_grace_ends() {
    let start = Instant::now();
    let mut draft = Draft::default();
    draft.note(b"run it", false, start);
    draft.note(b"\r", false, start);
    assert_eq!(draft, Draft::Submitted(start));
    assert!(draft.pending(start + Duration::from_secs(1)));
    assert!(!draft.pending(start + SUBMIT_GRACE));
    draft.taken_up();
    assert!(!draft.pending(start));
    // Enter on an empty prompt submits nothing
    assert_eq!(typed(&[b"\r"]), Draft::Clean);
    // typing again after a submit starts a new draft
    let mut again = Draft::Submitted(start);
    again.note(b"x", false, start);
    assert_eq!(again, Draft::Typed(1));
}

#[test]
fn a_clean_prompt_is_not_pending_and_any_text_is() {
    let now = Instant::now();
    assert!(!Draft::Clean.pending(now));
    assert!(Draft::Typed(1).pending(now));
    assert!(Draft::Unknown.pending(now));
}

fn message(id: &str, handle: &str, queued_at: u64, expires_at: u64) -> Stored {
    Stored {
        record: MessageRecord {
            id: id.into(),
            status: Status::Queued,
            reason: Some(Reason::Working),
            pane_id: "terminal_1".into(),
            handle: handle.into(),
            session: "work".into(),
            agent: Some("claude".into()),
            from: "boxa/work/brisk-heron".into(),
            from_name: Some("builder".into()),
            from_tab: None,
            from_person: None,
            reply_to: None,
            queued_at,
            expires_at,
            delivered_at: None,
            via: None,
        },
        text: "Run the tests.".into(),
    }
}

#[test]
fn held_messages_survive_a_restart_in_their_order() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("messages").join("work");
    let mut mailbox = Mailbox::open(path.clone(), 1_000);
    mailbox.hold(message("mb", "sunny-otter", 20, 100_000));
    mailbox.hold(message("ma", "sunny-otter", 10, 100_000));
    mailbox.hold(message("mc", "brisk-heron", 30, 100_000));
    mailbox.set_reason("mb", Reason::Draft, Some("terminal_4".into()));
    assert_eq!(mailbox.held_for("sunny-otter"), 2);

    let restored = Mailbox::open(path.clone(), 2_000);
    let ids: Vec<&str> = restored
        .held()
        .iter()
        .map(|s| s.record.id.as_str())
        .collect();
    assert_eq!(ids, ["ma", "mb", "mc"], "oldest first");
    let mb = restored.get("mb").unwrap();
    assert_eq!(mb.record.reason, Some(Reason::Draft));
    assert_eq!(mb.record.pane_id, "terminal_4");
    assert_eq!(mb.text, "Run the tests.");
    // nothing is left half-written
    assert!(std::fs::read_dir(&path)
        .unwrap()
        .all(|e| e.unwrap().path().extension().unwrap() == "json"));
}

#[test]
fn a_message_expires_at_its_time_even_when_no_server_was_running() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    let mut mailbox = Mailbox::open(path.clone(), 0);
    mailbox.hold(message("ma", "sunny-otter", 0, 5_000));
    mailbox.hold(message("mb", "sunny-otter", 0, 50_000));
    assert!(mailbox.expire(4_999).is_empty());
    assert_eq!(mailbox.expire(5_000), vec!["ma".to_owned()]);
    assert_eq!(mailbox.get("ma").unwrap().record.status, Status::Expired);
    drop(mailbox);

    let restored = Mailbox::open(path, 60_000);
    assert!(restored.is_empty());
    assert_eq!(restored.get("mb").unwrap().record.status, Status::Expired);
    // the reason it waited on stays on the record
    assert_eq!(
        restored.get("mb").unwrap().record.reason,
        Some(Reason::Working)
    );
}

#[test]
fn a_delivered_message_is_remembered_for_a_reply_and_listed_after_the_held_ones() {
    let dir = tempfile::tempdir().unwrap();
    let mut mailbox = Mailbox::open(dir.path().to_path_buf(), 0);
    mailbox.hold(message("ma", "sunny-otter", 0, 50_000));
    mailbox.hold(message("mb", "sunny-otter", 1, 50_000));
    let delivered = mailbox.settle("ma", Status::Delivered, 777).unwrap();
    assert_eq!(delivered.status, Status::Delivered);
    assert_eq!(delivered.delivered_at, Some(777));
    assert_eq!(delivered.reason, None);
    let records = mailbox.records();
    assert_eq!(records[0].id, "mb");
    assert_eq!(records[1].id, "ma");
    let restored = Mailbox::open(dir.path().to_path_buf(), 1_000);
    assert_eq!(
        restored.get("ma").unwrap().record.from,
        "boxa/work/brisk-heron"
    );
}

#[test]
fn old_settled_messages_are_forgotten_and_a_broken_file_is_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    let mut mailbox = Mailbox::open(path.clone(), 0);
    mailbox.hold(message("ma", "sunny-otter", 0, 50_000));
    mailbox.settle("ma", Status::Delivered, 1_000);
    std::fs::write(path.join("broken.json"), b"{not json").unwrap();
    let week = 7 * 24 * 60 * 60 * 1000;
    let restored = Mailbox::open(path.clone(), 1_000 + week + 1);
    assert!(restored.get("ma").is_none());
    assert!(!path.join("ma.json").exists());
}

#[test]
fn new_ids_count_up_per_session_and_survive_a_restart() {
    let mut mailbox = Mailbox::default();
    assert_eq!(mailbox.new_id(), "1");
    assert_eq!(mailbox.new_id(), "2");

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();
    let mut mailbox = Mailbox::open(path.clone(), 0);
    let first = mailbox.new_id();
    assert_eq!(first, "1");
    mailbox.hold(message(&first, "sunny-otter", 0, 50_000));
    let second = mailbox.new_id();
    assert_eq!(second, "2");
    // a new server for the same session goes on from the saved counter, even for an id it
    // handed out and never stored
    let mut restored = Mailbox::open(path.clone(), 1);
    assert_eq!(restored.new_id(), "3");
    // the counter file lost: the ids still on disk keep the next one clear of them
    std::fs::remove_file(path.join("next-id")).unwrap();
    let mut restored = Mailbox::open(path.clone(), 1);
    assert_eq!(restored.new_id(), "2");
    // an id an older build gave is still found, and the counter steps past an id in use
    restored.hold(message("m3k9x2q", "sunny-otter", 0, 50_000));
    restored.hold(message("3", "sunny-otter", 0, 50_000));
    assert!(restored.get("m3k9x2q").is_some());
    assert_eq!(restored.new_id(), "4");
}

#[test]
fn a_cancelled_message_leaves_the_held_ones_and_stays_cancelled_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let mut mailbox = Mailbox::open(dir.path().to_path_buf(), 0);
    mailbox.hold(message("1", "sunny-otter", 0, 50_000));
    let cancelled = mailbox.cancel("1", 10).unwrap();
    assert_eq!(cancelled.status, Status::Cancelled);
    assert_eq!(cancelled.reason, None);
    assert!(mailbox.held().is_empty());
    // a second cancel finds it settled
    assert_eq!(
        mailbox.cancel("1", 11).unwrap_err().map(|r| r.status),
        Some(Status::Cancelled)
    );
    let restored = Mailbox::open(dir.path().to_path_buf(), 20);
    assert!(restored.held().is_empty());
    assert_eq!(restored.get("1").unwrap().record.status, Status::Cancelled);
    assert_eq!(restored.get("1").unwrap().text, "Run the tests.");
}

#[test]
fn an_expired_message_yields_one_notice_and_a_notice_yields_none() {
    use zellij_utils::agent_mail::expiry_notice;
    let mut mailbox = Mailbox::default();
    mailbox.hold(message("3", "sunny-otter", 0, 60_000));
    let mut notice = message("4", "brisk-heron", 0, 60_000);
    notice.record.from = "boxa/-/zellij".into();
    mailbox.hold(notice);
    let expired = mailbox.expire(60_000);
    assert_eq!(expired, vec!["3".to_owned(), "4".to_owned()]);
    // the settled record keeps its last reason, which the notice names
    let (to, _, text) = expiry_notice(&mailbox.get("3").unwrap().record, "boxa", "work").unwrap();
    assert_eq!(to, "brisk-heron");
    assert!(text.contains("the agent stayed busy"), "{text}");
    assert_eq!(
        expiry_notice(&mailbox.get("4").unwrap().record, "boxa", "work"),
        None
    );
    // settled once: a later round expires nothing again
    assert!(mailbox.expire(120_000).is_empty());
}
