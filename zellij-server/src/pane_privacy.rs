//! Deciding which panes this session keeps to itself.
//!
//! The settings are read from `config.kdl` into [`PanePrivacyOptions`], which is plain data in
//! `zellij-utils` and has to stay that way - it is parsed by code that builds for wasm. Everything
//! that cannot build for wasm is here: reading the patterns file, compiling the regular
//! expressions, and testing a pane row against them.
//!
//! **One evaluation point.** The policy is built once when the session starts and answered from
//! `route.rs`, which is where every CLI verb arrives. Nothing decides this question anywhere else,
//! because a second copy of the rule is a second chance to disagree with the first - and the way it
//! would disagree is by showing a pane that the other copy hides.
//!
//! **Fail closed.** A policy that names a file it cannot read withholds everything and says so in
//! the log. A filter that quietly is not there is the failure mode worth spending a whole session's
//! usefulness to avoid.

use std::collections::HashSet;
use std::path::Path;

use regex::RegexSet;
use zellij_utils::data::PaneListEntry;
use zellij_utils::input::options::Options;
use zellij_utils::pane_privacy::{
    MatchField, OnUnknownCwd, PanePrivacyOptions, TabRule, PANE_PRIVACY_FILE_ENV,
};

/// A pane, addressed the way the pane list addresses one.
///
/// `PaneId` from `zellij-utils` would do, but this keeps the module free of the conversions between
/// the three pane id types the tree carries, and a `(bool, u32)` is what the row itself holds.
pub type PaneKey = (bool, u32);

fn key_of(entry: &PaneListEntry) -> PaneKey {
    (entry.pane_info.is_plugin, entry.pane_info.id)
}

/// Which panes and tabs of one pane list are withheld.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Verdicts {
    pub panes: HashSet<PaneKey>,
    pub tabs: HashSet<usize>,
}

impl Verdicts {
    pub fn withholds_pane(&self, is_plugin: bool, id: u32) -> bool {
        self.panes.contains(&(is_plugin, id))
    }
    pub fn withholds_tab(&self, tab_id: usize) -> bool {
        self.tabs.contains(&tab_id)
    }
    pub fn is_empty(&self) -> bool {
        self.panes.is_empty() && self.tabs.is_empty()
    }
}

/// The compiled policy for a session.
#[derive(Debug)]
pub enum PanePrivacy {
    /// No `pane_privacy` block, or one that names no pattern. Every question short-circuits here.
    Off,
    On(Policy),
    /// The settings named a file that could not be read, or a pattern that would not compile.
    ///
    /// Everything is withheld, which is the only answer that cannot leak. The reason is in the log
    /// rather than in the refusal: a refusal is read by whoever called, and the person who can fix
    /// a broken policy is the one reading the session log.
    Broken(String),
}

#[derive(Debug)]
pub struct Policy {
    patterns: RegexSet,
    fields: Vec<MatchField>,
    on_unknown_cwd: OnUnknownCwd,
    tab_rule: TabRule,
}

impl PanePrivacy {
    /// Build the policy a session will answer with, for the whole life of that session.
    ///
    /// The environment is read here and nowhere below, so that the rest of this module can be
    /// tested without one. `ZELLIJ_PANE_PRIVACY_FILE` wins over the config file: which directories
    /// are private is a fact about a machine, and one `config.kdl` is shared across several.
    pub fn from_options(options: &Options) -> PanePrivacy {
        let mut settings = options.pane_privacy.clone().unwrap_or_default();
        if let Some(from_env) = std::env::var(PANE_PRIVACY_FILE_ENV)
            .ok()
            .filter(|value| !value.trim().is_empty())
        {
            settings.patterns_file = Some(std::path::PathBuf::from(from_env));
        }
        Self::build(&settings, |path| std::fs::read_to_string(path))
    }

    /// The same, with the file read supplied - so the whole of this module can be tested with no
    /// disk and no environment.
    pub fn build(
        settings: &PanePrivacyOptions,
        read_file: impl Fn(&Path) -> std::io::Result<String>,
    ) -> PanePrivacy {
        let patterns_file = settings.patterns_file.clone();

        if patterns_file.is_none() && settings.patterns.is_empty() {
            return PanePrivacy::Off;
        }

        let mut sources: Vec<String> = Vec::new();
        if let Some(path) = &patterns_file {
            match read_file(path) {
                Ok(contents) => match patterns_of(&contents) {
                    Ok(patterns) => sources.extend(patterns),
                    Err(message) => {
                        return PanePrivacy::Broken(format!("{}: {}", path.display(), message))
                    },
                },
                Err(e) => {
                    return PanePrivacy::Broken(format!(
                        "could not read the pane privacy patterns file {}: {}",
                        path.display(),
                        e
                    ))
                },
            }
        }
        sources.extend(settings.patterns.iter().cloned());

        if sources.is_empty() {
            // a file that holds only comments asks for nothing, and a session that withholds
            // nothing should not pay for a filter
            return PanePrivacy::Off;
        }

        // case-insensitive, because a path typed in the config and a path reported by the session
        // differ in case often enough that a rule missing for that reason is a rule that looks like
        // it works
        let anchored: Vec<String> = sources.iter().map(|line| format!("(?i){}", line)).collect();
        match RegexSet::new(&anchored) {
            Ok(patterns) => PanePrivacy::On(Policy {
                patterns,
                fields: settings.match_fields_or_default(),
                on_unknown_cwd: settings.on_unknown_cwd_or_default(),
                tab_rule: settings.tab_rule_or_default(),
            }),
            Err(e) => {
                PanePrivacy::Broken(format!("a pane privacy pattern does not compile: {}", e))
            },
        }
    }

    /// Whether anything is being withheld at all. `false` is the whole cost when the filter is off.
    pub fn is_active(&self) -> bool {
        !matches!(self, PanePrivacy::Off)
    }

    /// Why the policy is broken, if it is. For the log, never for a refusal.
    pub fn broken_reason(&self) -> Option<&str> {
        match self {
            PanePrivacy::Broken(reason) => Some(reason.as_str()),
            _ => None,
        }
    }

    /// What this pane list gives away, decided once for the whole list.
    ///
    /// The tab rule is why this takes the list rather than a row: whether a tab is withheld is a
    /// question about its siblings, and a row on its own cannot answer it.
    pub fn verdicts(&self, entries: &[PaneListEntry]) -> Verdicts {
        let mut verdicts = Verdicts::default();
        let policy = match self {
            PanePrivacy::Off => return verdicts,
            PanePrivacy::Broken(_) => {
                // fail closed: every pane and every tab in the list
                for entry in entries {
                    verdicts.panes.insert(key_of(entry));
                    verdicts.tabs.insert(entry.pane_info.tab_id);
                }
                return verdicts;
            },
            PanePrivacy::On(policy) => policy,
        };

        let mut matched_by_tab: Vec<(usize, bool)> = Vec::new();
        for entry in entries {
            let matched = policy.matches_row(entry);
            if matched {
                verdicts.panes.insert(key_of(entry));
            }
            matched_by_tab.push((entry.pane_info.tab_id, matched));
        }

        let tab_ids: HashSet<usize> = matched_by_tab.iter().map(|(tab_id, _)| *tab_id).collect();
        for tab_id in tab_ids {
            let rows = matched_by_tab.iter().filter(|(id, _)| *id == tab_id);
            let withheld = match policy.tab_rule {
                TabRule::Any => rows.clone().any(|(_, matched)| *matched),
                TabRule::All => rows.clone().all(|(_, matched)| *matched),
            };
            if withheld {
                verdicts.tabs.insert(tab_id);
            }
        }

        // a withheld tab takes its panes with it, whichever rule decided it
        for entry in entries {
            if verdicts.tabs.contains(&entry.pane_info.tab_id) {
                verdicts.panes.insert(key_of(entry));
            }
        }
        verdicts
    }

    /// The rows a caller may see, and how many were dropped.
    pub fn filter(&self, entries: Vec<PaneListEntry>) -> (Vec<PaneListEntry>, usize) {
        if !self.is_active() {
            return (entries, 0);
        }
        let verdicts = self.verdicts(&entries);
        let before = entries.len();
        let kept: Vec<PaneListEntry> = entries
            .into_iter()
            .filter(|entry| !verdicts.panes.contains(&key_of(entry)))
            .collect();
        let withheld = before - kept.len();
        (kept, withheld)
    }

    /// Whether a directory a caller wants a new pane in is one the policy names.
    ///
    /// This is the one question asked of a path rather than of a pane. Without it an agent that
    /// cannot see a private pane opens its own shell in the private directory and reads it there.
    pub fn withholds_cwd(&self, cwd: &Path) -> bool {
        match self {
            PanePrivacy::Off => false,
            PanePrivacy::Broken(_) => true,
            PanePrivacy::On(policy) => {
                // a cwd is tested whatever `match_fields` says, because `match_fields` is about
                // which columns of a pane row describe a pane, and this is not a pane row
                policy.patterns.is_match(&cwd.to_string_lossy())
            },
        }
    }
}

impl Policy {
    fn matches_row(&self, entry: &PaneListEntry) -> bool {
        let info = &entry.pane_info;
        for field in &self.fields {
            let matched = match field {
                MatchField::Cwd => match info.pane_cwd.as_ref() {
                    Some(cwd) => self.patterns.is_match(cwd),
                    // a plugin pane has no cwd by construction and never gains one, so withholding
                    // it for not having one would withhold the status bar for ever
                    None if info.is_plugin => false,
                    None => self.on_unknown_cwd == OnUnknownCwd::Withhold,
                },
                MatchField::Command => [
                    info.pane_command.as_deref(),
                    info.terminal_command.as_deref(),
                ]
                .into_iter()
                .flatten()
                .any(|command| self.patterns.is_match(command)),
                MatchField::Title => [Some(info.title.as_str()), info.program_title.as_deref()]
                    .into_iter()
                    .flatten()
                    .any(|title| self.patterns.is_match(title)),
                MatchField::TabName => self.patterns.is_match(&entry.tab_name),
            };
            if matched {
                return true;
            }
        }
        false
    }
}

/// The patterns of a patterns file: one extended regular expression per line.
///
/// `#` comments and blank lines are dropped. A line that does not compile is an error naming the
/// line number, because a privacy rule that was silently skipped is worse than no rule at all - the
/// user believes they are covered.
fn patterns_of(contents: &str) -> Result<Vec<String>, String> {
    let mut patterns = Vec::new();
    for (index, line) in contents.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Err(e) = regex::Regex::new(line) {
            return Err(format!("line {} is not a regex: {}", index + 1, e));
        }
        patterns.push(line.to_owned());
    }
    Ok(patterns)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zellij_utils::data::{PaneInfo, PaneListEntry};

    fn pane(id: u32, tab_id: usize, cwd: Option<&str>) -> PaneListEntry {
        let mut info = PaneInfo::default();
        info.id = id;
        info.title = format!("pane {}", id);
        info.pane_cwd = cwd.map(String::from);
        info.tab_id = tab_id;
        PaneListEntry {
            pane_info: info,
            tab_position: tab_id,
            tab_name: format!("tab {}", tab_id),
            agent: None,
            command_state: None,
            last_command_exit_code: None,
        }
    }

    fn plugin_pane(id: u32, tab_id: usize) -> PaneListEntry {
        let mut entry = pane(id, tab_id, None);
        entry.pane_info.is_plugin = true;
        entry
    }

    fn built(settings: PanePrivacyOptions) -> PanePrivacy {
        PanePrivacy::build(&settings, |_| {
            Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no file in this test",
            ))
        })
    }

    fn with_patterns(patterns: &[&str]) -> PanePrivacy {
        built(PanePrivacyOptions {
            patterns: patterns.iter().map(|p| p.to_string()).collect(),
            ..Default::default()
        })
    }

    #[test]
    fn no_settings_is_off() {
        assert!(!built(PanePrivacyOptions::default()).is_active());
    }

    #[test]
    fn an_empty_policy_keeps_every_row() {
        let policy = built(PanePrivacyOptions::default());
        let panes = vec![pane(1, 0, Some("/work/open")), pane(2, 0, None)];
        let (kept, withheld) = policy.filter(panes);
        assert_eq!(kept.len(), 2);
        assert_eq!(withheld, 0);
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let patterns = patterns_of("# a comment\n\n  \nreally-private\n").unwrap();
        assert_eq!(patterns, vec!["really-private".to_owned()]);
    }

    #[test]
    fn a_file_of_only_comments_leaves_the_filter_off() {
        let policy = PanePrivacy::build(
            &PanePrivacyOptions {
                patterns_file: Some("/invented/patterns.txt".into()),
                ..Default::default()
            },
            |_| Ok("# nothing here\n\n".to_owned()),
        );
        assert!(!policy.is_active());
    }

    #[test]
    fn a_line_that_is_not_a_regex_names_its_line_number() {
        let message = patterns_of("fine\n[unclosed\n").unwrap_err();
        assert!(message.starts_with("line 2 is not a regex"), "{}", message);
    }

    #[test]
    fn a_file_that_cannot_be_read_withholds_everything() {
        let policy = built(PanePrivacyOptions {
            patterns_file: Some("/invented/missing.txt".into()),
            ..Default::default()
        });
        assert!(policy.is_active());
        assert!(policy.broken_reason().is_some());
        let (kept, withheld) = policy.filter(vec![pane(1, 0, Some("/work/open"))]);
        assert!(kept.is_empty());
        assert_eq!(withheld, 1);
        assert!(policy.withholds_cwd(Path::new("/work/open")));
    }

    #[test]
    fn an_unanchored_fragment_matches_anywhere_in_the_path_and_ignores_case() {
        let policy = with_patterns(&["pretend-private"]);
        let panes = vec![
            pane(1, 0, Some("/invented/PRETEND-Private/notes")),
            pane(2, 1, Some("/invented/open/notes")),
        ];
        let (kept, withheld) = policy.filter(panes);
        assert_eq!(withheld, 1);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].pane_info.id, 2);
    }

    #[test]
    fn a_terminal_pane_with_no_cwd_is_withheld_by_default_and_a_plugin_pane_is_not() {
        let policy = with_patterns(&["pretend-private"]);
        let verdicts = policy.verdicts(&[pane(1, 0, None), plugin_pane(2, 1)]);
        assert!(verdicts.withholds_pane(false, 1));
        assert!(!verdicts.withholds_pane(true, 2));
    }

    #[test]
    fn on_unknown_cwd_allow_keeps_a_pane_with_no_cwd() {
        let policy = built(PanePrivacyOptions {
            patterns: vec!["pretend-private".to_owned()],
            on_unknown_cwd: Some(OnUnknownCwd::Allow),
            ..Default::default()
        });
        let verdicts = policy.verdicts(&[pane(1, 0, None)]);
        assert!(!verdicts.withholds_pane(false, 1));
    }

    #[test]
    fn tab_rule_any_takes_the_whole_tab() {
        let policy = with_patterns(&["pretend-private"]);
        let panes = vec![
            pane(1, 3, Some("/invented/pretend-private")),
            pane(2, 3, Some("/invented/open")),
            pane(3, 4, Some("/invented/open")),
        ];
        let (kept, withheld) = policy.filter(panes);
        assert_eq!(withheld, 2);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].pane_info.tab_id, 4);
    }

    #[test]
    fn tab_rule_all_takes_only_the_panes_that_matched() {
        let policy = built(PanePrivacyOptions {
            patterns: vec!["pretend-private".to_owned()],
            tab_rule: Some(TabRule::All),
            on_unknown_cwd: Some(OnUnknownCwd::Allow),
            ..Default::default()
        });
        let panes = vec![
            pane(1, 3, Some("/invented/pretend-private")),
            pane(2, 3, Some("/invented/open")),
        ];
        let verdicts = policy.verdicts(&panes);
        assert!(!verdicts.withholds_tab(3));
        assert!(verdicts.withholds_pane(false, 1));
        assert!(!verdicts.withholds_pane(false, 2));
    }

    #[test]
    fn match_fields_gates_which_columns_are_tried() {
        let mut entry = pane(1, 0, Some("/invented/open"));
        entry.pane_info.title = "pretend-private notes".to_owned();

        let default_fields = with_patterns(&["pretend-private"]);
        assert!(!default_fields
            .verdicts(&[entry.clone()])
            .withholds_pane(false, 1));

        let with_title = built(PanePrivacyOptions {
            patterns: vec!["pretend-private".to_owned()],
            match_fields: Some(vec![MatchField::Title]),
            ..Default::default()
        });
        assert!(with_title.verdicts(&[entry]).withholds_pane(false, 1));
    }

    #[test]
    fn the_command_column_is_tried_by_default() {
        let mut entry = pane(1, 0, Some("/invented/open"));
        entry.pane_info.pane_command = Some("secret-tool --run".to_owned());
        let policy = with_patterns(&["secret-tool"]);
        assert!(policy.verdicts(&[entry]).withholds_pane(false, 1));
    }

    #[test]
    fn a_tab_name_is_only_tried_when_it_is_asked_for() {
        let mut entry = pane(1, 0, Some("/invented/open"));
        entry.tab_name = "pretend-private".to_owned();
        assert!(!with_patterns(&["pretend-private"])
            .verdicts(&[entry.clone()])
            .withholds_pane(false, 1));
        let with_tab_name = built(PanePrivacyOptions {
            patterns: vec!["pretend-private".to_owned()],
            match_fields: Some(vec![MatchField::TabName]),
            ..Default::default()
        });
        assert!(with_tab_name.verdicts(&[entry]).withholds_pane(false, 1));
    }

    #[test]
    fn a_requested_cwd_is_tested_against_the_patterns() {
        let policy = with_patterns(&["pretend-private"]);
        assert!(policy.withholds_cwd(Path::new("/invented/pretend-private/work")));
        assert!(!policy.withholds_cwd(Path::new("/invented/open/work")));
    }

    #[test]
    fn an_inline_pattern_that_does_not_compile_breaks_the_policy_closed() {
        let policy = with_patterns(&["[unclosed"]);
        assert!(policy.is_active());
        assert!(policy.broken_reason().is_some());
    }

    #[test]
    fn a_file_and_inline_patterns_are_both_used() {
        let policy = PanePrivacy::build(
            &PanePrivacyOptions {
                patterns_file: Some("/invented/patterns.txt".into()),
                patterns: vec!["from-config".to_owned()],
                ..Default::default()
            },
            |_| Ok("from-file\n".to_owned()),
        );
        assert!(policy.withholds_cwd(Path::new("/invented/from-file")));
        assert!(policy.withholds_cwd(Path::new("/invented/from-config")));
    }
}
