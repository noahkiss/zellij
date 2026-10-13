//! Shared harness facts and pure terminal-state evaluation.
//!
//! No server wiring or filesystem discovery occurs here. Facts describe pinned
//! sources, not every installed release. Callers must inspect `verified`.

pub mod evaluator;

use serde::{Deserialize, Serialize};

/// Provenance belongs to each fact, including facts awaiting verification.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Fact {
    pub key: String,
    pub value: String,
    pub source: String,
    pub revision: String,
    pub verified: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub notes: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Harness {
    pub schema_version: u32,
    pub harness: String,
    pub facts: Vec<Fact>,
}

/// A consumer can reconstruct the detector from the same exported document.
#[derive(Debug, Clone, Serialize)]
pub struct DetectionManifest {
    pub harness: &'static str,
    pub source: &'static str,
    pub revision: &'static str,
    pub toml: &'static str,
    pub local_toml: Option<&'static str>,
    pub local_source: &'static str,
    pub local_revision: &'static str,
    pub local_verified: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Inventory {
    pub schema_version: u32,
    pub harnesses: Vec<Harness>,
    pub detection_manifests: Vec<DetectionManifest>,
}

const FACT_FILES: [(&str, &str); 4] = [
    (
        "claude",
        include_str!("../../assets/harness/inventory/claude.toml"),
    ),
    (
        "codex",
        include_str!("../../assets/harness/inventory/codex.toml"),
    ),
    (
        "opencode",
        include_str!("../../assets/harness/inventory/opencode.toml"),
    ),
    ("pi", include_str!("../../assets/harness/inventory/pi.toml")),
];

const MANIFEST_FILES: [(&str, &str, Option<&str>); 4] = [
    (
        "claude",
        include_str!("../../assets/harness/herdr/claude.toml"),
        Some(include_str!("../../assets/harness/local/claude.toml")),
    ),
    (
        "codex",
        include_str!("../../assets/harness/herdr/codex.toml"),
        Some(include_str!("../../assets/harness/local/codex.toml")),
    ),
    (
        "opencode",
        include_str!("../../assets/harness/herdr/opencode.toml"),
        None,
    ),
    (
        "pi",
        include_str!("../../assets/harness/herdr/pi.toml"),
        Some(include_str!("../../assets/harness/local/pi.toml")),
    ),
];

pub fn inventory() -> anyhow::Result<Inventory> {
    let harnesses = FACT_FILES
        .iter()
        .map(|(name, text)| {
            let harness: Harness = toml::from_str(text)?;
            anyhow::ensure!(
                harness.schema_version == 1 && harness.harness == *name,
                "invalid harness inventory header: {name}"
            );
            let mut keys = std::collections::HashSet::new();
            for fact in &harness.facts {
                anyhow::ensure!(
                    !fact.key.is_empty()
                        && !fact.value.is_empty()
                        && !fact.source.is_empty()
                        && !fact.revision.is_empty(),
                    "missing harness fact provenance: {name}"
                );
                anyhow::ensure!(
                    keys.insert(&fact.key),
                    "duplicate harness fact: {name}/{}",
                    fact.key
                );
            }
            Ok(harness)
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let detection_manifests = MANIFEST_FILES.iter().map(|(name, text, local)| DetectionManifest {
        harness: name,
        source: "https://github.com/herdrdev/herdr/tree/87e5c13c6028df72405f63388dde585212ad3019/src/detect/manifests",
        revision: "87e5c13c6028df72405f63388dde585212ad3019",
        toml: text,
        local_toml: *local,
        local_source: "zellij fork: zellij-utils/assets/harness/local/",
        local_revision: "2026.10.13.1",
        local_verified: false,
    }).collect();
    Ok(Inventory {
        schema_version: 1,
        harnesses,
        detection_manifests,
    })
}

impl Inventory {
    pub fn to_text(&self) -> String {
        let mut output = String::from("Harness map (schema 1)\n");
        for harness in &self.harnesses {
            output.push_str(&format!("\n{}\n", harness.harness));
            for fact in &harness.facts {
                output.push_str(&format!(
                    "  {}: {}{}\n    {} @ {}\n",
                    fact.key,
                    fact.value,
                    if fact.verified { "" } else { " [unverified]" },
                    fact.source,
                    fact.revision
                ));
                if !fact.notes.is_empty() {
                    output.push_str(&format!("    {}\n", fact.notes));
                }
            }
        }
        for manifest in &self.detection_manifests {
            output.push_str(&format!(
                "\n{} detector @ {}\n{}\n{}",
                manifest.harness,
                manifest.revision,
                manifest.toml,
                manifest.local_toml.unwrap_or("")
            ));
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn cli_accepts_harness_map_in_both_formats() {
        for flags in [
            vec!["zellij", "setup", "--harness-map"],
            vec!["zellij", "setup", "--harness-map", "--json"],
        ] {
            let args = crate::cli::CliArgs::try_parse_from(flags).unwrap();
            match args.command.unwrap() {
                crate::cli::Command::Setup(setup) => assert!(setup.harness_map),
                other => panic!("unexpected command: {other:?}"),
            }
        }
    }

    #[test]
    fn exported_inventory_contains_provenance_and_detectors() {
        let map = inventory().unwrap();
        assert_eq!(map.harnesses.len(), 4);
        let json = serde_json::to_value(&map).unwrap();
        assert_eq!(json["schema_version"], 1);
        for (harness, manifest) in map.harnesses.iter().zip(&map.detection_manifests) {
            assert_eq!(harness.harness, manifest.harness);
            assert!(harness.facts.iter().any(|fact| !fact.verified));
            evaluator::Manifest::parse(manifest.toml).unwrap();
            if let Some(local) = manifest.local_toml {
                evaluator::Manifest::parse(local).unwrap();
            }
        }
        assert!(map.to_text().contains("[unverified]"));
    }

    #[test]
    fn supplemental_screen_corpus() {
        #[derive(Deserialize)]
        struct Corpus {
            screens: Vec<Screen>,
        }
        #[derive(Deserialize)]
        struct Screen {
            harness: String,
            screen: String,
            state: String,
            rule: String,
        }
        let corpus: Corpus =
            toml::from_str(include_str!("../../assets/harness/local/fixtures.toml")).unwrap();
        for fixture in corpus.screens {
            let result = evaluate_screen(&fixture.harness, &fixture.screen).unwrap();
            assert_eq!(
                serde_json::to_value(result.state).unwrap(),
                fixture.state,
                "{}: {}",
                fixture.harness,
                fixture.screen
            );
            assert_eq!(
                result.matched_rule.as_deref().unwrap_or(""),
                fixture.rule,
                "{}: {}",
                fixture.harness,
                fixture.screen
            );
            if result.state == evaluator::State::Question {
                assert!(result.visible_blocker);
            }
        }
    }

    #[test]
    fn overlays_and_stronger_osc_evidence_keep_priority() {
        let detector = HarnessDetector::new("codex").unwrap().unwrap();
        let result = detector.evaluate(evaluator::DetectionInput {
            screen: "Select a scope\nenter to submit answer",
            osc_title: "Action Required",
            ..Default::default()
        });
        assert_eq!(result.matched_rule.as_deref(), Some("osc_title_blocked"));
        assert_eq!(result.state, evaluator::State::Blocked);
        let result = evaluate_screen("claude", "Showing detailed transcript\nctrl+o to toggle\nEnter to select · arrow keys to navigate · esc to cancel").unwrap();
        assert!(result.skip_state_update);
        assert_eq!(result.state, evaluator::State::Unknown);
        assert!(HarnessDetector::new("unrecognized").unwrap().is_none());
    }
}

/// Compile once per harness, then evaluate screen snapshots without side effects.
#[derive(Debug, Clone)]
pub struct HarnessDetector {
    primary: evaluator::Manifest,
    supplement: Option<evaluator::Manifest>,
}

impl HarnessDetector {
    pub fn new(label: &str) -> Result<Option<Self>, String> {
        let Some(primary) = evaluator::bundled_manifest(label)? else {
            return Ok(None);
        };
        let supplement = MANIFEST_FILES
            .iter()
            .find(|(name, _, _)| *name == primary.id())
            .and_then(|(_, _, local)| *local)
            .map(evaluator::Manifest::parse)
            .transpose()?;
        Ok(Some(Self {
            primary,
            supplement,
        }))
    }

    /// Skip-update overlays preserve state. Local rules only replace weaker evidence.
    /// Unmatched screens return unknown; the upstream evaluator exposes its idle fallback separately.
    pub fn evaluate(&self, input: evaluator::DetectionInput<'_>) -> evaluator::DetectionResult {
        let mut result = self.primary.evaluate(input);
        if result.skip_state_update {
            return result;
        }
        if let Some(supplement) = &self.supplement {
            let local = supplement.evaluate(input);
            if local.matched_rule.is_some()
                && (local.matched_priority > result.matched_priority
                    || (local.matched_priority == result.matched_priority
                        && local.state == evaluator::State::Question
                        && result.state == evaluator::State::Blocked))
            {
                return local;
            }
        }
        if result.idle_fallback {
            result.state = evaluator::State::Unknown;
        }
        result
    }
}

/// Screen-only entry point. Reuse HarnessDetector when polling many snapshots.
pub fn evaluate_screen(label: &str, screen: &str) -> Result<evaluator::DetectionResult, String> {
    match HarnessDetector::new(label)? {
        Some(detector) => Ok(detector.evaluate(evaluator::DetectionInput {
            screen,
            ..Default::default()
        })),
        None => evaluator::evaluate(label, screen),
    }
}
