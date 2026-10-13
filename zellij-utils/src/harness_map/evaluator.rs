//! Pure screen detection, ported from herdr at the revision in assets/harness/herdr/NOTICE.
//! The caller supplies rendered screen text, without terminal escape sequences.
use regex::Regex;
use serde::{Deserialize, Serialize};

/// Highest herdr manifest engine version this evaluator implements.
pub const ENGINE_VERSION: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Idle,
    Working,
    Blocked,
    Question,
    Unknown,
}

/// OSC fields remain empty when the caller cannot supply them.
#[derive(Debug, Clone, Copy, Default)]
pub struct DetectionInput<'a> {
    pub screen: &'a str,
    pub osc_title: &'a str,
    pub osc_progress: &'a str,
}

/// Evidence flags and skip_state_update preserve upstream's update semantics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DetectionResult {
    pub state: State,
    pub matched_rule: Option<String>,
    pub matched_priority: Option<i32>,
    pub visible_idle: bool,
    pub visible_blocker: bool,
    pub visible_working: bool,
    pub skip_state_update: bool,
    /// Herdr defaults a known harness to idle without visible idle evidence.
    pub idle_fallback: bool,
}
impl DetectionResult {
    fn unknown() -> Self {
        Self {
            state: State::Unknown,
            matched_rule: None,
            matched_priority: None,
            visible_idle: false,
            visible_blocker: false,
            visible_working: false,
            skip_state_update: false,
            idle_fallback: false,
        }
    }
}

/// Parsed, validated, and compiled manifest. Reuse it for repeated screen polling.
#[derive(Debug, Clone)]
pub struct Manifest {
    manifest: AgentManifest,
    compiled_rules: Vec<CompiledRule>,
}
impl Manifest {
    pub fn parse(content: &str) -> Result<Self, String> {
        let mut manifest: AgentManifest = toml::from_str(content).map_err(|e| e.to_string())?;
        if manifest
            .min_engine_version
            .is_some_and(|v| v > ENGINE_VERSION)
        {
            return Err(format!(
                "manifest requires engine {:?}; current engine is {ENGINE_VERSION}",
                manifest.min_engine_version
            ));
        }
        if let Some(version) = manifest.version.as_mut() {
            *version = validate_version(version)?;
        }
        validate_manifest(&manifest)?;
        let compiled_rules = compile_manifest(&manifest)?;
        Ok(Self {
            manifest,
            compiled_rules,
        })
    }
    pub fn id(&self) -> &str {
        &self.manifest.id
    }
    pub fn version(&self) -> Option<&str> {
        self.manifest.version.as_deref()
    }
    pub fn aliases(&self) -> &[String] {
        &self.manifest.aliases
    }
    pub fn evaluate(&self, input: DetectionInput<'_>) -> DetectionResult {
        let mut selected: Option<&ManifestRule> = None;
        for (rule, compiled) in self.manifest.rules.iter().zip(&self.compiled_rules) {
            if compiled_rule_matches(compiled, region(input, &rule.region))
                && selected.map_or(true, |previous| previous.priority < rule.priority)
            {
                selected = Some(rule);
            }
        }
        let Some(rule) = selected else {
            return DetectionResult {
                state: State::Idle,
                idle_fallback: true,
                ..DetectionResult::unknown()
            };
        };
        let state = match rule.state {
            Some(ManifestState::Idle) => State::Idle,
            Some(ManifestState::Working) => State::Working,
            Some(ManifestState::Blocked) => State::Blocked,
            Some(ManifestState::Question) => State::Question,
            _ => State::Unknown,
        };
        DetectionResult {
            state,
            matched_rule: Some(rule.id.clone()),
            matched_priority: Some(rule.priority),
            visible_idle: rule.visible_idle && state == State::Idle,
            visible_blocker: rule.visible_blocker
                && matches!(state, State::Blocked | State::Question),
            visible_working: rule.visible_working && state == State::Working,
            skip_state_update: rule.skip_state_update,
            idle_fallback: false,
        }
    }
}

/// Return the verbatim bundled source, including its engine requirements.
pub fn bundled_source(label: &str) -> Option<&'static str> {
    match label {
        "claude" | "claude-code" => Some(include_str!("../../assets/harness/herdr/claude.toml")),
        "codex" => Some(include_str!("../../assets/harness/herdr/codex.toml")),
        "opencode" | "open-code" | "herdr:opencode" => {
            Some(include_str!("../../assets/harness/herdr/opencode.toml"))
        },
        "pi" | "herdr:pi" => Some(include_str!("../../assets/harness/herdr/pi.toml")),
        _ => None,
    }
}
pub fn bundled_manifest(label: &str) -> Result<Option<Manifest>, String> {
    bundled_source(label).map(Manifest::parse).transpose()
}
/// Unknown harnesses return unknown. Invalid manifests return an error, never guessed state.
pub fn evaluate(label: &str, screen: &str) -> Result<DetectionResult, String> {
    Ok(match bundled_manifest(label)? {
        Some(manifest) => manifest.evaluate(DetectionInput {
            screen,
            ..Default::default()
        }),
        None => DetectionResult::unknown(),
    })
}
// Same dotted-numeric version acceptance as herdr's ManifestVersion::parse.
fn validate_version(value: &str) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err("version must not be empty".to_string());
    }
    for segment in trimmed.split('.') {
        if segment.is_empty() {
            return Err(format!("version {trimmed:?} contains an empty segment"));
        }
        if !segment.chars().all(|ch| ch.is_ascii_digit()) {
            return Err(format!("version {trimmed:?} must be dotted numeric"));
        }
        segment
            .parse::<u64>()
            .map_err(|_| format!("version {trimmed:?} contains an oversized segment"))?;
    }
    Ok(trimmed.to_string())
}
const MAX_RULES_PER_MANIFEST: usize = 128;
const MAX_GATE_DEPTH: usize = 8;
const MAX_TOTAL_GATES: usize = 512;
const MAX_MATCHERS_PER_GATE: usize = 32;
const MAX_TOTAL_MATCHERS: usize = 1024;
const MAX_MATCHER_CHARS: usize = 512;
#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct AgentManifest {
    id: String,
    version: Option<String>,
    min_engine_version: Option<u32>,
    #[serde(rename = "updated_at")]
    _updated_at: Option<String>,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    rules: Vec<ManifestRule>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct ManifestRule {
    id: String,
    state: Option<ManifestState>,
    #[serde(default)]
    priority: i32,
    #[serde(default = "default_region")]
    region: String,
    #[serde(default)]
    visible_idle: bool,
    #[serde(default)]
    visible_blocker: bool,
    #[serde(default)]
    visible_working: bool,
    #[serde(default)]
    skip_state_update: bool,
    #[serde(default)]
    all: Vec<ManifestGate>,
    #[serde(default)]
    any: Vec<ManifestGate>,
    #[serde(default, rename = "not")]
    not_gate: Vec<ManifestGate>,
    #[serde(default)]
    contains: Vec<String>,
    #[serde(default)]
    regex: Vec<String>,
    #[serde(default)]
    line_regex: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct ManifestGate {
    #[serde(default)]
    all: Vec<ManifestGate>,
    #[serde(default)]
    any: Vec<ManifestGate>,
    #[serde(default, rename = "not")]
    not_gate: Vec<ManifestGate>,
    #[serde(default)]
    contains: Vec<String>,
    #[serde(default)]
    regex: Vec<String>,
    #[serde(default)]
    line_regex: Vec<String>,
}

#[derive(Debug, Clone)]
struct CompiledRule {
    gate: CompiledGate,
}

#[derive(Debug, Clone)]
struct CompiledGate {
    all: Vec<CompiledGate>,
    any: Vec<CompiledGate>,
    not_gate: Vec<CompiledGate>,
    contains: Vec<String>,
    regex: Vec<Regex>,
    line_regex: Vec<Regex>,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ManifestState {
    Question,
    Idle,
    Working,
    Blocked,
    Unknown,
}

fn default_region() -> String {
    "whole_recent".to_string()
}
fn validate_manifest(manifest: &AgentManifest) -> Result<(), String> {
    if manifest.rules.is_empty() {
        return Err("manifest must contain at least one rule".to_string());
    }
    if manifest.rules.len() > MAX_RULES_PER_MANIFEST {
        return Err(format!(
            "manifest contains {} rules, max is {MAX_RULES_PER_MANIFEST}",
            manifest.rules.len()
        ));
    }

    let mut complexity = ManifestComplexity::default();
    for rule in &manifest.rules {
        if rule.id.trim().is_empty() {
            return Err("manifest rule id must not be empty".to_string());
        }
        if rule.skip_state_update {
            if rule.state != Some(ManifestState::Unknown) {
                return Err(format!(
                    "rule {} uses skip_state_update without state = \"unknown\"",
                    rule.id
                ));
            }
            if rule.visible_idle || rule.visible_blocker || rule.visible_working {
                return Err(format!(
                    "rule {} uses skip_state_update with visible state evidence",
                    rule.id
                ));
            }
        }
        validate_region_name(&rule.region)
            .map_err(|err| format!("rule {} uses invalid region: {err}", rule.id))?;
        if rule.region.trim().starts_with("top_non_empty_lines(")
            && manifest
                .min_engine_version
                .is_some_and(|version| version < TOP_NON_EMPTY_LINES_ENGINE_VERSION)
        {
            return Err(format!(
                "rule {} uses top_non_empty_lines but min_engine_version is below {}",
                rule.id, TOP_NON_EMPTY_LINES_ENGINE_VERSION
            ));
        }
        validate_rule_gate(rule, &mut complexity)
            .map_err(|err| format!("rule {} has invalid matcher gates: {err}", rule.id))?;
    }

    Ok(())
}

#[derive(Default)]
struct ManifestComplexity {
    total_gates: usize,
    total_matchers: usize,
}

fn validate_rule_gate(
    rule: &ManifestRule,
    complexity: &mut ManifestComplexity,
) -> Result<(), String> {
    validate_gate(&manifest_gate_from_rule(rule), "rule", 0, complexity)
}

fn validate_gate(
    gate: &ManifestGate,
    context: &str,
    depth: usize,
    complexity: &mut ManifestComplexity,
) -> Result<(), String> {
    if depth > MAX_GATE_DEPTH {
        return Err(format!("{context} exceeds max gate depth {MAX_GATE_DEPTH}"));
    }
    complexity.total_gates += 1;
    if complexity.total_gates > MAX_TOTAL_GATES {
        return Err(format!("manifest exceeds max gate count {MAX_TOTAL_GATES}"));
    }
    validate_matcher_limits(gate, context, complexity)?;
    if !gate_has_positive_matcher(gate) {
        return Err(format!("{context} must contain a positive matcher"));
    }
    validate_regex_patterns(&gate.regex, context, "regex")?;
    validate_regex_patterns(&gate.line_regex, context, "line_regex")?;
    for nested in &gate.all {
        validate_gate(nested, "all gate", depth + 1, complexity)?;
    }
    for nested in &gate.any {
        validate_gate(nested, "any gate", depth + 1, complexity)?;
    }
    for nested in &gate.not_gate {
        if !gate_has_any_matcher(nested) {
            return Err(format!("{context} contains an empty not gate"));
        }
        validate_not_gate(nested, depth + 1, complexity)?;
    }
    Ok(())
}

fn validate_not_gate(
    gate: &ManifestGate,
    depth: usize,
    complexity: &mut ManifestComplexity,
) -> Result<(), String> {
    if depth > MAX_GATE_DEPTH {
        return Err(format!("not gate exceeds max gate depth {MAX_GATE_DEPTH}"));
    }
    complexity.total_gates += 1;
    if complexity.total_gates > MAX_TOTAL_GATES {
        return Err(format!("manifest exceeds max gate count {MAX_TOTAL_GATES}"));
    }
    validate_matcher_limits(gate, "not gate", complexity)?;
    if !gate_has_any_matcher(gate) {
        return Err("not gate must contain a matcher".to_string());
    }
    validate_regex_patterns(&gate.regex, "not gate", "regex")?;
    validate_regex_patterns(&gate.line_regex, "not gate", "line_regex")?;
    for nested in &gate.all {
        validate_gate(nested, "not all gate", depth + 1, complexity)?;
    }
    for nested in &gate.any {
        validate_gate(nested, "not any gate", depth + 1, complexity)?;
    }
    for nested in &gate.not_gate {
        validate_not_gate(nested, depth + 1, complexity)?;
    }
    Ok(())
}

fn validate_matcher_limits(
    gate: &ManifestGate,
    context: &str,
    complexity: &mut ManifestComplexity,
) -> Result<(), String> {
    let matcher_count = gate.contains.len() + gate.regex.len() + gate.line_regex.len();
    if matcher_count > MAX_MATCHERS_PER_GATE {
        return Err(format!(
            "{context} has {matcher_count} direct matchers, max is {MAX_MATCHERS_PER_GATE}"
        ));
    }
    complexity.total_matchers += matcher_count;
    if complexity.total_matchers > MAX_TOTAL_MATCHERS {
        return Err(format!(
            "manifest exceeds max matcher count {MAX_TOTAL_MATCHERS}"
        ));
    }
    for value in gate
        .contains
        .iter()
        .chain(gate.regex.iter())
        .chain(gate.line_regex.iter())
    {
        if value.chars().count() > MAX_MATCHER_CHARS {
            return Err(format!(
                "{context} matcher exceeds max length {MAX_MATCHER_CHARS}"
            ));
        }
    }
    Ok(())
}

fn validate_regex_patterns(patterns: &[String], context: &str, field: &str) -> Result<(), String> {
    for pattern in patterns {
        Regex::new(pattern).map_err(|err| {
            format!("{context} contains invalid {field} pattern {pattern:?}: {err}")
        })?;
    }
    Ok(())
}

fn gate_has_positive_matcher(gate: &ManifestGate) -> bool {
    !gate.contains.is_empty()
        || !gate.regex.is_empty()
        || !gate.line_regex.is_empty()
        || !gate.all.is_empty()
        || !gate.any.is_empty()
}

fn gate_has_any_matcher(gate: &ManifestGate) -> bool {
    gate_has_positive_matcher(gate) || !gate.not_gate.is_empty()
}

fn validate_region_name(spec: &str) -> Result<(), String> {
    let trimmed = spec.trim();
    match trimmed {
        "whole_recent"
        | "after_last_prompt_marker"
        | "before_current_prompt_marker"
        | "whole_recent_without_current_prompt_marker"
        | "current_prompt_block_marker"
        | "after_current_prompt_block_marker"
        | "prompt_box_body"
        | "above_prompt_box"
        | "last_non_empty_above_prompt_box"
        | "after_last_horizontal_rule"
        | "osc_title"
        | "osc_progress" => Ok(()),
        _ if region_count(trimmed, "bottom_lines").is_some()
            || region_count(trimmed, "bottom_non_empty_lines").is_some()
            || top_region_count(trimmed).is_some() =>
        {
            Ok(())
        },
        _ => Err(trimmed.to_string()),
    }
}

fn manifest_gate_from_rule(rule: &ManifestRule) -> ManifestGate {
    ManifestGate {
        all: rule.all.clone(),
        any: rule.any.clone(),
        not_gate: rule.not_gate.clone(),
        contains: rule.contains.clone(),
        regex: rule.regex.clone(),
        line_regex: rule.line_regex.clone(),
    }
}

fn compile_manifest(manifest: &AgentManifest) -> Result<Vec<CompiledRule>, String> {
    manifest
        .rules
        .iter()
        .map(|rule| {
            compile_gate(&manifest_gate_from_rule(rule))
                .map(|gate| CompiledRule { gate })
                .map_err(|err| format!("rule {} could not be compiled: {err}", rule.id))
        })
        .collect()
}

fn compile_gate(gate: &ManifestGate) -> Result<CompiledGate, String> {
    Ok(CompiledGate {
        all: gate
            .all
            .iter()
            .map(compile_gate)
            .collect::<Result<_, _>>()?,
        any: gate
            .any
            .iter()
            .map(compile_gate)
            .collect::<Result<_, _>>()?,
        not_gate: gate
            .not_gate
            .iter()
            .map(compile_gate)
            .collect::<Result<_, _>>()?,
        contains: gate
            .contains
            .iter()
            .map(|needle| needle.to_lowercase())
            .collect(),
        regex: gate
            .regex
            .iter()
            .map(|pattern| Regex::new(pattern).map_err(|err| err.to_string()))
            .collect::<Result<_, _>>()?,
        line_regex: gate
            .line_regex
            .iter()
            .map(|pattern| Regex::new(pattern).map_err(|err| err.to_string()))
            .collect::<Result<_, _>>()?,
    })
}

fn compiled_rule_matches(rule: &CompiledRule, text: &str) -> bool {
    let lower_text = text.to_lowercase();
    compiled_gate_matches(&rule.gate, text, &lower_text)
}

fn compiled_gate_matches(gate: &CompiledGate, text: &str, lower_text: &str) -> bool {
    if !gate
        .contains
        .iter()
        .all(|needle| lower_text.contains(needle))
    {
        return false;
    }

    if !gate.regex.iter().all(|regex| regex.is_match(text)) {
        return false;
    }

    if !gate
        .line_regex
        .iter()
        .all(|regex| text.lines().any(|line| regex.is_match(line)))
    {
        return false;
    }

    if !gate
        .all
        .iter()
        .all(|nested| compiled_gate_matches(nested, text, lower_text))
    {
        return false;
    }

    if !gate.any.is_empty()
        && !gate
            .any
            .iter()
            .any(|nested| compiled_gate_matches(nested, text, lower_text))
    {
        return false;
    }

    if gate
        .not_gate
        .iter()
        .any(|nested| compiled_gate_matches(nested, text, lower_text))
    {
        return false;
    }

    true
}

fn region<'a>(input: DetectionInput<'a>, spec: &str) -> &'a str {
    let trimmed = spec.trim();
    // OSC regions source from their dedicated fields, not the screen.
    match trimmed {
        "osc_title" => return input.osc_title,
        "osc_progress" => return input.osc_progress,
        _ => {},
    }
    // All other regions operate on the screen content as before.
    let content = input.screen;
    match trimmed {
        "whole_recent" => content,
        "after_last_prompt_marker" => after_last_prompt_marker(content),
        "before_current_prompt_marker" => before_current_prompt_marker(content),
        "whole_recent_without_current_prompt_marker" => {
            whole_recent_without_current_prompt_marker(content)
        },
        "current_prompt_block_marker" => current_prompt_block_marker(content).unwrap_or(""),
        "after_current_prompt_block_marker" => {
            after_current_prompt_block_marker(content).unwrap_or("")
        },
        "prompt_box_body" => prompt_box_body(content).unwrap_or(""),
        "above_prompt_box" => above_prompt_box(content),
        "last_non_empty_above_prompt_box" => last_non_empty_line(above_prompt_box(content)),
        "after_last_horizontal_rule" => after_last_horizontal_rule(content),
        _ => {
            if let Some(count) = region_count(trimmed, "bottom_lines") {
                return bottom_lines(content, count);
            }
            if let Some(count) = region_count(trimmed, "bottom_non_empty_lines") {
                return bottom_non_empty_lines(content, count);
            }
            if let Some(count) = top_region_count(trimmed) {
                return top_non_empty_lines(content, count);
            }
            ""
        },
    }
}

fn region_count(spec: &str, name: &str) -> Option<usize> {
    spec.strip_prefix(name)
        .and_then(|rest| rest.strip_prefix('('))
        .and_then(|rest| rest.strip_suffix(')'))
        .and_then(|count| count.parse::<usize>().ok())
}

const TOP_NON_EMPTY_LINES_ENGINE_VERSION: u32 = 3;
const MAX_TOP_REGION_LINE_COUNT: usize = u16::MAX as usize;

fn top_region_count(spec: &str) -> Option<usize> {
    let count = spec
        .strip_prefix("top_non_empty_lines")?
        .strip_prefix('(')?
        .strip_suffix(')')?;
    if count.starts_with('0') || !count.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    count
        .parse::<usize>()
        .ok()
        .filter(|count| *count <= MAX_TOP_REGION_LINE_COUNT)
}

fn bottom_lines(content: &str, count: usize) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    let start = lines.len().saturating_sub(count);
    slice_from_line_index(content, &lines, start)
}

fn bottom_non_empty_lines(content: &str, count: usize) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    let Some(start_index) = lines
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, line)| !line.trim().is_empty())
        .take(count)
        .last()
        .map(|(index, _)| index)
    else {
        return "";
    };
    slice_from_line_index(content, &lines, start_index)
}

fn top_non_empty_lines(content: &str, count: usize) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    let Some(end_index) = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .take(count)
        .last()
        .map(|(index, _)| index)
    else {
        return "";
    };
    let byte_offset = line_start_offset(content, &lines, end_index + 1);
    &content[..byte_offset]
}

fn after_last_prompt_marker(content: &str) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    let Some(index) = lines.iter().rposition(|line| codex_prompt_line(line)) else {
        return content;
    };
    slice_from_line_index(content, &lines, index + 1)
}

fn before_current_prompt_marker(content: &str) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    let Some(index) = current_codex_prompt_index(&lines) else {
        return content;
    };
    let byte_offset = lines[..index]
        .iter()
        .map(|line| line.len() + 1)
        .sum::<usize>();
    &content[..byte_offset.min(content.len())]
}

fn whole_recent_without_current_prompt_marker(content: &str) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    if current_codex_prompt_index(&lines).is_some() {
        ""
    } else {
        content
    }
}

fn current_prompt_block_marker(content: &str) -> Option<&str> {
    let lines: Vec<&str> = content.lines().collect();
    let prompt_index = current_codex_prompt_index(&lines)?;
    lines[..prompt_index]
        .iter()
        .rev()
        .find(|line| codex_block_marker_line(line))
        .copied()
}

fn after_current_prompt_block_marker(content: &str) -> Option<&str> {
    let lines: Vec<&str> = content.lines().collect();
    let prompt_index = current_codex_prompt_index(&lines)?;
    let block_index = lines[..prompt_index]
        .iter()
        .rposition(|line| codex_block_marker_line(line))?;
    Some(slice_from_line_index(content, &lines, block_index))
}

fn current_codex_prompt_index(lines: &[&str]) -> Option<usize> {
    let prompt_index = lines.iter().rposition(|line| codex_prompt_line(line))?;
    if lines[prompt_index + 1..]
        .iter()
        .any(|line| codex_block_marker_line(line))
    {
        return None;
    }
    Some(prompt_index)
}

fn codex_prompt_line(line: &str) -> bool {
    line == "›" || line.starts_with("› ")
}

fn codex_block_marker_line(line: &str) -> bool {
    line.starts_with('•') || line.starts_with('■') || line.starts_with('✗') || line.starts_with('✓')
}

fn prompt_box_body(content: &str) -> Option<&str> {
    let lines: Vec<&str> = content.lines().collect();
    let top = prompt_box_top_border_index(&lines)?;
    let start = line_start_offset(content, &lines, top + 1);
    let end_index = lines[top + 1..]
        .iter()
        .position(|line| is_horizontal_rule(line))
        .map(|relative| top + 1 + relative)
        .unwrap_or(lines.len());
    let end = line_start_offset(content, &lines, end_index);
    Some(&content[start.min(content.len())..end.min(content.len())])
}

fn above_prompt_box(content: &str) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    let Some(top) = prompt_box_top_border_index(&lines) else {
        return content;
    };
    let end = line_start_offset(content, &lines, top);
    &content[..end.min(content.len())]
}

fn after_last_horizontal_rule(content: &str) -> &str {
    let mut last_rule_end = 0usize;
    let mut offset = 0usize;
    for line in content.lines() {
        let next_offset = offset + line.len() + 1;
        if is_horizontal_rule(line) {
            last_rule_end = next_offset.min(content.len());
        }
        offset = next_offset;
    }
    &content[last_rule_end..]
}

fn last_non_empty_line(content: &str) -> &str {
    content
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
}

fn prompt_box_top_border_index(lines: &[&str]) -> Option<usize> {
    let mut border_count = 0;
    for index in (0..lines.len()).rev() {
        if is_horizontal_rule(lines[index]) {
            border_count += 1;
            if border_count == 2 {
                return Some(index);
            }
        }
    }
    None
}

fn is_horizontal_rule(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }

    let rule_chars = trimmed.chars().take_while(|&ch| ch == '─').count();
    if rule_chars == 0 {
        return false;
    }

    let rule_bytes = trimmed
        .char_indices()
        .nth(rule_chars)
        .map(|(index, _)| index)
        .unwrap_or(trimmed.len());
    let suffix = trimmed[rule_bytes..].trim_start();

    suffix.is_empty() || rule_chars >= 3
}

fn slice_from_line_index<'a>(content: &'a str, lines: &[&str], index: usize) -> &'a str {
    let byte_offset = line_start_offset(content, lines, index);
    &content[byte_offset.min(content.len())..]
}

fn line_start_offset(content: &str, lines: &[&str], index: usize) -> usize {
    lines[..index.min(lines.len())]
        .iter()
        .map(|line| line.len() + 1)
        .sum::<usize>()
        .min(content.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn screen_regions_extract_structure_without_classifying_agent_state() {
        for (screen, spec, expected) in [
            ("old\n\nnew\n", "bottom_lines(2)", "\nnew\n"),
            (
                "before\n› input\nafter\n",
                "after_last_prompt_marker",
                "after\n",
            ),
            (
                "before\n› input\nafter\n",
                "before_current_prompt_marker",
                "before\n",
            ),
            (
                "before\n› input\nafter\n",
                "whole_recent_without_current_prompt_marker",
                "",
            ),
            (
                "no marker\n",
                "whole_recent_without_current_prompt_marker",
                "no marker\n",
            ),
            (
                "• old\n■ latest\n› input\n",
                "current_prompt_block_marker",
                "■ latest",
            ),
            (
                "• old\n■ latest\n› input\n",
                "after_current_prompt_block_marker",
                "■ latest\n› input\n",
            ),
            ("› old\n• new\n", "current_prompt_block_marker", ""),
            (
                "above\n\n───\nbody\n───\nfooter\n",
                "above_prompt_box",
                "above\n\n",
            ),
            (
                "above\n\n───\nbody\n───\nfooter\n",
                "last_non_empty_above_prompt_box",
                "above",
            ),
            (
                "above\n───\nbody\n───\nfooter\n",
                "prompt_box_body",
                "body\n",
            ),
            (
                "above\n───\nbody\n───\nfooter\n",
                "after_last_horizontal_rule",
                "footer\n",
            ),
        ] {
            assert_eq!(
                region(
                    DetectionInput {
                        screen,
                        osc_title: "",
                        osc_progress: ""
                    },
                    spec
                ),
                expected,
                "region={spec}"
            );
        }
    }

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
    #[test]
    fn bundled_screen_corpus() {
        let corpus: Corpus =
            toml::from_str(include_str!("../../assets/harness/herdr/fixtures.toml")).unwrap();
        for fixture in corpus.screens {
            let result = evaluate(&fixture.harness, &fixture.screen).unwrap();
            assert_eq!(
                serde_json::to_value(result.state).unwrap(),
                fixture.state,
                "{}",
                fixture.harness
            );
            assert_eq!(
                result.matched_rule.as_deref().unwrap_or(""),
                fixture.rule,
                "{}",
                fixture.harness
            );
            assert_eq!(result.idle_fallback, fixture.rule.is_empty());
        }
    }
    #[test]
    fn unsupported_manifest_never_guesses() {
        for text in [
            "id = 'test'\nmin_engine_version = 4\n[[rules]]\nid = 'x'\ncontains = ['yes']",
            "id = 'test'\n[[rules]]\nid = 'x'\nregion = 'unsupported'\ncontains = ['yes']",
            "id = 'test'\n[[rules]]\nid = 'x'\ncontains = ['yes']\nfuture_gate = true",
            "id = 'test'\n[[rules]]\nid = 'x'\nregex = ['[']",
            "id = 'test'\n[[rules]]\nid = 'x'\nnot = [{contains=['yes']}]",
        ] {
            assert!(Manifest::parse(text).is_err(), "{text}");
        }
        assert_eq!(
            evaluate("unrecognized", "Working...").unwrap().state,
            State::Unknown
        );
    }
    #[test]
    fn nested_gates_case_insensitivity_and_priority_ties() {
        let manifest = Manifest::parse(
            r#"
id = 'test'
[[rules]]
id = 'first'
state = 'working'
priority = 10
contains = ['MATCH']
all = [{any = [{regex=['w[io]n']},{contains=['fallback']}]}]
not = [{contains=['blocked']}]
[[rules]]
id = 'second'
state = 'blocked'
priority = 10
contains = ['match']
"#,
        )
        .unwrap();
        assert_eq!(
            manifest
                .evaluate(DetectionInput {
                    screen: "match win",
                    ..Default::default()
                })
                .matched_rule
                .as_deref(),
            Some("first")
        );
        assert_eq!(
            manifest
                .evaluate(DetectionInput {
                    screen: "match win BLOCKED",
                    ..Default::default()
                })
                .matched_rule
                .as_deref(),
            Some("second")
        );
    }
    #[test]
    fn osc_and_overlay_evidence() {
        let manifest = bundled_manifest("codex").unwrap().unwrap();
        let result = manifest.evaluate(DetectionInput {
            screen: "Working (2s)",
            osc_title: "Action Required",
            osc_progress: "",
        });
        assert_eq!(result.state, State::Blocked);
        assert!(result.visible_blocker);
        let result = evaluate("claude", "Showing detailed transcript\nctrl+o to toggle").unwrap();
        assert!(result.skip_state_update);
        assert_eq!(result.state, State::Unknown);
    }
    #[test]
    fn version_and_structured_region_requirements() {
        for version in ["", "future", "1..2", "18446744073709551616"] {
            assert!(validate_version(version).is_err());
        }
        assert_eq!(validate_version(" 2026.10.11.1 ").unwrap(), "2026.10.11.1");
        let input = DetectionInput {
            screen: "\nfirst\n\nsecond\nthird",
            ..Default::default()
        };
        assert_eq!(
            region(input, "top_non_empty_lines(2)"),
            "\nfirst\n\nsecond\n"
        );
        assert_eq!(region(input, "bottom_non_empty_lines(2)"), "second\nthird");
        for region in [
            "top_non_empty_lines(0)",
            "top_non_empty_lines(01)",
            "top_non_empty_lines(65536)",
        ] {
            assert!(validate_region_name(region).is_err());
        }
        assert!(Manifest::parse("id='test'\nmin_engine_version=2\n[[rules]]\nid='test'\nregion='top_non_empty_lines(2)'\ncontains=['yes']").is_err());
        assert!(Manifest::parse("id='test'\n[[rules]]\nid='test'\nstate='working'\nskip_state_update=true\ncontains=['yes']").is_err());
    }
}
