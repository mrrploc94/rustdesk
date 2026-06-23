//! Input method configuration parser and pretty-printer.
//!
//! This module parses custom Vietnamese input-method configuration files in
//! either JSON or TOML format into an [`InputMethodConfig`] object, validating
//! required fields and enforcing a maximum file size of 10 MB.
//!
//! The full rule-validation logic (cycle and conflict detection) lives in
//! [`ConfigParser::validate_rules`]; conflict/cycle detection is completed by a
//! later task. Re-serialization back to JSON/TOML is provided by
//! [`ConfigPrettyPrinter`], whose output round-trips through
//! [`ConfigParser::parse`].
//!
//! Requirements covered here: 16.1, 16.2, 16.3, 16.4, 16.5, 16.8, 16.9, 16.10.

use serde_derive::{Deserialize, Serialize};

/// Maximum accepted configuration file size: 10 MB.
pub const MAX_CONFIG_FILE_SIZE: usize = 10 * 1024 * 1024;

/// Supported configuration file formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigFormat {
    /// JSON-formatted configuration.
    Json,
    /// TOML-formatted configuration.
    Toml,
}

/// A parsed custom input-method configuration.
///
/// The `id`, `name`, and `rules` fields are required; `metadata` is optional.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InputMethodConfig {
    /// Stable unique identifier for the input method.
    pub id: String,
    /// Human-readable display name.
    pub name: String,
    /// The list of transformation rules.
    pub rules: Vec<TransformationRule>,
    /// Optional metadata (version, author, description).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<ConfigMetadata>,
}

/// A single transformation rule mapping an input sequence to an output char.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransformationRule {
    /// The raw keystroke sequence that triggers this rule (e.g. "aa").
    pub input_sequence: String,
    /// The resulting Unicode character (e.g. 'â').
    pub output_char: char,
    /// Optional context constraints for when this rule applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<RuleContext>,
}

/// Context constraints that gate when a [`TransformationRule`] applies.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RuleContext {
    /// Characters that must immediately precede the sequence, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preceding: Option<Vec<char>>,
    /// Characters that must NOT immediately precede the sequence, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub not_preceding: Option<Vec<char>>,
}

/// Optional descriptive metadata for an input-method configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigMetadata {
    /// Configuration schema/version string.
    pub version: String,
    /// Optional author attribution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// Optional free-form description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Errors that can occur while parsing a configuration file.
#[derive(Debug, Clone, PartialEq)]
pub enum ParseError {
    /// A syntax error in the underlying JSON/TOML, with a 1-based line number.
    SyntaxError {
        /// 1-based line number where the error occurred (0 if unknown).
        line: usize,
        /// Descriptive error message from the underlying parser.
        message: String,
    },
    /// A required field (`id`, `name`, or `rules`) was missing.
    MissingField {
        /// The name of the missing field.
        field: String,
    },
    /// A specific transformation rule was invalid.
    InvalidRule {
        /// Index of the offending rule within the `rules` array.
        rule_index: usize,
        /// Descriptive message explaining why the rule is invalid.
        message: String,
    },
    /// The configuration file exceeded the maximum allowed size.
    FileTooLarge {
        /// The actual size of the input, in bytes.
        size: usize,
        /// The maximum permitted size, in bytes.
        max: usize,
    },
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::SyntaxError { line, message } => {
                write!(f, "syntax error at line {}: {}", line, message)
            }
            ParseError::MissingField { field } => {
                write!(f, "missing required field: `{}`", field)
            }
            ParseError::InvalidRule {
                rule_index,
                message,
            } => write!(f, "invalid rule at index {}: {}", rule_index, message),
            ParseError::FileTooLarge { size, max } => write!(
                f,
                "configuration file too large: {} bytes (maximum {} bytes)",
                size, max
            ),
        }
    }
}

impl std::error::Error for ParseError {}

/// Errors produced by rule validation (cycle / conflict detection).
///
/// Full detection logic is implemented by a later task; the variants are
/// defined here so dependent code can compile and pattern-match against them.
#[derive(Debug, Clone, PartialEq)]
pub enum ValidationError {
    /// A set of rules form a transformation cycle.
    CyclicRule {
        /// Indices of the rules participating in the cycle.
        rules: Vec<usize>,
    },
    /// The same input sequence maps to multiple different output characters.
    ConflictingMapping {
        /// The conflicting input sequence.
        input: String,
        /// The differing output characters produced for that input.
        outputs: Vec<char>,
    },
}

/// Parses input-method configuration files into [`InputMethodConfig`].
pub struct ConfigParser;

impl ConfigParser {
    /// Parse `input` in the given `format` into an [`InputMethodConfig`].
    ///
    /// Returns [`ParseError::FileTooLarge`] if the input exceeds 10 MB,
    /// [`ParseError::MissingField`] if a required field is absent, and
    /// [`ParseError::SyntaxError`] (with a line number where available) for
    /// malformed input.
    pub fn parse(input: &str, format: ConfigFormat) -> Result<InputMethodConfig, ParseError> {
        if input.len() > MAX_CONFIG_FILE_SIZE {
            return Err(ParseError::FileTooLarge {
                size: input.len(),
                max: MAX_CONFIG_FILE_SIZE,
            });
        }

        match format {
            ConfigFormat::Json => Self::parse_json(input),
            ConfigFormat::Toml => Self::parse_toml(input),
        }
    }

    /// Validate that the configuration's transformation rules contain no
    /// conflicting mappings or cycles.
    ///
    /// Two kinds of problems are detected, and *all* problems are collected
    /// into the returned `Vec` (validation does not stop at the first error):
    ///
    /// 1. **Conflicting mappings** (Requirements 16.6, 16.7). Per the
    ///    requirement, a conflict is "two rules that map the same input
    ///    sequence to different output characters". Rules are grouped by their
    ///    *applicability key* — the `input_sequence` together with the
    ///    [`RuleContext`]. Two rules that share the same input sequence but
    ///    apply under different contexts are NOT in conflict (they never both
    ///    fire for the same keystrokes), so they are excluded. When a single
    ///    key yields more than one distinct `output_char`, a
    ///    [`ValidationError::ConflictingMapping`] is emitted carrying the input
    ///    sequence and the distinct outputs (in first-seen order).
    ///
    /// 2. **Cyclic rules** (Requirement 16.6). The rules are treated as the
    ///    edges of a directed graph whose nodes are keystroke strings: each
    ///    rule contributes an edge from its `input_sequence` to its
    ///    `output_char` (as a one-character string). A cycle exists when, by
    ///    repeatedly feeding one rule's output into another rule's input, the
    ///    chain returns to a sequence it already visited. Each distinct cycle
    ///    is reported once as a [`ValidationError::CyclicRule`] listing the
    ///    indices of the participating rules.
    ///
    /// Returns `Ok(())` when no problems are found.
    pub fn validate_rules(config: &InputMethodConfig) -> Result<(), Vec<ValidationError>> {
        let mut errors = Vec::new();
        errors.extend(detect_conflicts(&config.rules));
        errors.extend(detect_cycles(&config.rules));

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    fn parse_json(input: &str) -> Result<InputMethodConfig, ParseError> {
        match serde_json::from_str::<InputMethodConfig>(input) {
            Ok(config) => Ok(config),
            Err(e) => {
                let message = e.to_string();
                if e.is_data() {
                    if let Some(field) = extract_missing_field(&message) {
                        return Err(ParseError::MissingField { field });
                    }
                }
                Err(ParseError::SyntaxError {
                    line: e.line(),
                    message,
                })
            }
        }
    }

    fn parse_toml(input: &str) -> Result<InputMethodConfig, ParseError> {
        match toml::from_str::<InputMethodConfig>(input) {
            Ok(config) => Ok(config),
            Err(e) => {
                let message = e.message().to_string();
                if let Some(field) = extract_missing_field(&message) {
                    return Err(ParseError::MissingField { field });
                }
                let line = e
                    .span()
                    .map(|span| line_from_byte_offset(input, span.start))
                    .unwrap_or(0);
                Err(ParseError::SyntaxError { line, message })
            }
        }
    }
}

/// Serializes [`InputMethodConfig`] objects back into valid JSON or TOML text.
///
/// The produced output is guaranteed to round-trip: feeding the result of
/// [`ConfigPrettyPrinter::format`] back into [`ConfigParser::parse`] with the
/// same [`ConfigFormat`] reconstructs a structurally identical
/// [`InputMethodConfig`]. Optional fields that are `None` are omitted entirely
/// (rather than emitted as `null`), which keeps the TOML output valid since
/// TOML has no null representation.
///
/// Requirements covered: 16.4, 16.5.
pub struct ConfigPrettyPrinter;

impl ConfigPrettyPrinter {
    /// Format `config` into pretty-printed text in the given `format`.
    ///
    /// The output is valid, human-readable JSON or TOML and is re-parseable by
    /// [`ConfigParser::parse`]. Serialization of these data structures cannot
    /// fail, so this returns a plain `String`.
    pub fn format(config: &InputMethodConfig, format: ConfigFormat) -> String {
        match format {
            ConfigFormat::Json => serde_json::to_string_pretty(config)
                .expect("InputMethodConfig serializes to JSON infallibly"),
            ConfigFormat::Toml => toml::to_string_pretty(config)
                .expect("InputMethodConfig serializes to TOML infallibly"),
        }
    }
}

/// A key identifying when a rule applies: its input sequence plus context.
///
/// Two rules share a key only when they would fire for the exact same
/// keystrokes in the exact same surrounding context. Rules that differ in
/// context are never simultaneously applicable and therefore cannot conflict.
type ApplicabilityKey = (String, Option<RuleContext>);

/// Detect conflicting mappings among `rules`.
///
/// Rules are grouped by their [`ApplicabilityKey`]. A group that produces more
/// than one distinct `output_char` is a conflict, reported once with the
/// distinct outputs in first-seen order. Conflicts are returned in the order
/// their input sequences are first encountered, for deterministic output.
fn detect_conflicts(rules: &[TransformationRule]) -> Vec<ValidationError> {
    // Preserve first-seen ordering of keys while accumulating distinct outputs.
    let mut order: Vec<ApplicabilityKey> = Vec::new();
    let mut outputs_by_key: std::collections::HashMap<ApplicabilityKey, Vec<char>> =
        std::collections::HashMap::new();

    for rule in rules {
        let key = (rule.input_sequence.clone(), rule.context.clone());
        let entry = outputs_by_key.entry(key.clone()).or_insert_with(|| {
            order.push(key.clone());
            Vec::new()
        });
        if !entry.contains(&rule.output_char) {
            entry.push(rule.output_char);
        }
    }

    let mut errors = Vec::new();
    for key in order {
        let outputs = &outputs_by_key[&key];
        if outputs.len() > 1 {
            errors.push(ValidationError::ConflictingMapping {
                input: key.0,
                outputs: outputs.clone(),
            });
        }
    }
    errors
}

/// Detect cyclic rules by treating each rule as a directed graph edge.
///
/// Each rule contributes an edge `input_sequence -> output_char` (the output
/// rendered as a one-character string). A cycle is any path that revisits a
/// node, i.e. following outputs back into inputs eventually loops. Each
/// distinct cycle (identified by its set of participating rule indices) is
/// reported once.
fn detect_cycles(rules: &[TransformationRule]) -> Vec<ValidationError> {
    use std::collections::HashMap;

    // Build adjacency: node -> list of (target_node, rule_index).
    let mut adjacency: HashMap<String, Vec<(String, usize)>> = HashMap::new();
    for (idx, rule) in rules.iter().enumerate() {
        let target = rule.output_char.to_string();
        adjacency
            .entry(rule.input_sequence.clone())
            .or_default()
            .push((target, idx));
    }

    // Node colors for iterative cycle detection: White (unvisited),
    // Gray (on the current DFS path), Black (fully explored).
    #[derive(Clone, Copy, PartialEq)]
    enum Color {
        White,
        Gray,
        Black,
    }

    let mut color: HashMap<String, Color> = HashMap::new();
    // Deduplicate cycles by the sorted set of rule indices involved.
    let mut seen_cycles: std::collections::HashSet<Vec<usize>> = std::collections::HashSet::new();
    let mut errors = Vec::new();

    // Stack frames carry the node and a cursor over its outgoing edges.
    // The Gray chain (and the rule indices that built it) is tracked in `path`.
    struct Frame {
        node: String,
        next_edge: usize,
    }

    // Iterate over rule order so reported cycles are deterministic.
    let mut roots: Vec<String> = Vec::new();
    let mut seen_root: std::collections::HashSet<String> = std::collections::HashSet::new();
    for rule in rules {
        if seen_root.insert(rule.input_sequence.clone()) {
            roots.push(rule.input_sequence.clone());
        }
    }

    for root in roots {
        if matches!(color.get(&root), Some(Color::Black)) {
            continue;
        }

        let mut stack: Vec<Frame> = vec![Frame {
            node: root.clone(),
            next_edge: 0,
        }];
        // `path` holds (node, enter_rule) for every Gray frame on the stack.
        let mut path: Vec<(String, Option<usize>)> = Vec::new();

        color.insert(root.clone(), Color::Gray);
        path.push((root.clone(), None));

        while !stack.is_empty() {
            let top = stack.len() - 1;
            let node = stack[top].node.clone();
            let edges = adjacency.get(&node).cloned().unwrap_or_default();
            let edge_cursor = stack[top].next_edge;

            if edge_cursor < edges.len() {
                stack[top].next_edge += 1;
                let (target, rule_idx) = edges[edge_cursor].clone();

                match color.get(&target).copied().unwrap_or(Color::White) {
                    Color::White => {
                        color.insert(target.clone(), Color::Gray);
                        path.push((target.clone(), Some(rule_idx)));
                        stack.push(Frame {
                            node: target,
                            next_edge: 0,
                        });
                    }
                    Color::Gray => {
                        // Back edge: target is somewhere on the current path.
                        // Collect rule indices from target's position onward,
                        // then add the closing edge's rule index.
                        if let Some(start) =
                            path.iter().position(|(n, _)| n == &target)
                        {
                            let mut cycle_rules: Vec<usize> = path[start + 1..]
                                .iter()
                                .filter_map(|(_, r)| *r)
                                .collect();
                            cycle_rules.push(rule_idx);

                            let mut dedup_key = cycle_rules.clone();
                            dedup_key.sort_unstable();
                            dedup_key.dedup();
                            if seen_cycles.insert(dedup_key) {
                                errors.push(ValidationError::CyclicRule {
                                    rules: cycle_rules,
                                });
                            }
                        }
                    }
                    Color::Black => {}
                }
            } else {
                // Exhausted this node's edges: mark Black and pop.
                color.insert(node, Color::Black);
                path.pop();
                stack.pop();
            }
        }
    }

    errors
}

/// Extract the field name from a parser "missing field `x`" message.
fn extract_missing_field(message: &str) -> Option<String> {
    let marker = "missing field";
    let idx = message.find(marker)?;
    let rest = &message[idx + marker.len()..];
    let start = rest.find('`')? + 1;
    let end = rest[start..].find('`')? + start;
    Some(rest[start..end].to_string())
}

/// Compute a 1-based line number for a byte offset within `input`.
fn line_from_byte_offset(input: &str, offset: usize) -> usize {
    let clamped = offset.min(input.len());
    input[..clamped].bytes().filter(|&b| b == b'\n').count() + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_JSON: &str = r#"
    {
        "id": "custom-telex",
        "name": "Custom Telex",
        "rules": [
            { "input_sequence": "aa", "output_char": "â" },
            { "input_sequence": "aw", "output_char": "ă",
              "context": { "preceding": ["a"] } }
        ],
        "metadata": { "version": "1.0", "author": "user" }
    }
    "#;

    const VALID_TOML: &str = r#"
        id = "custom-vni"
        name = "Custom VNI"

        [[rules]]
        input_sequence = "o6"
        output_char = "ô"

        [[rules]]
        input_sequence = "o7"
        output_char = "ơ"

        [metadata]
        version = "2.0"
        description = "A custom VNI variant"
    "#;

    #[test]
    fn parses_valid_json_config() {
        let config = ConfigParser::parse(VALID_JSON, ConfigFormat::Json)
            .expect("valid JSON should parse");
        assert_eq!(config.id, "custom-telex");
        assert_eq!(config.name, "Custom Telex");
        assert_eq!(config.rules.len(), 2);
        assert_eq!(config.rules[0].input_sequence, "aa");
        assert_eq!(config.rules[0].output_char, 'â');
        // Second rule carries a preceding context constraint.
        let ctx = config.rules[1]
            .context
            .as_ref()
            .expect("second rule should have context");
        assert_eq!(ctx.preceding.as_ref().unwrap(), &vec!['a']);
        let meta = config.metadata.expect("metadata present");
        assert_eq!(meta.version, "1.0");
        assert_eq!(meta.author.as_deref(), Some("user"));
    }

    #[test]
    fn parses_valid_toml_config() {
        let config = ConfigParser::parse(VALID_TOML, ConfigFormat::Toml)
            .expect("valid TOML should parse");
        assert_eq!(config.id, "custom-vni");
        assert_eq!(config.name, "Custom VNI");
        assert_eq!(config.rules.len(), 2);
        assert_eq!(config.rules[1].input_sequence, "o7");
        assert_eq!(config.rules[1].output_char, 'ơ');
        let meta = config.metadata.expect("metadata present");
        assert_eq!(meta.version, "2.0");
        assert_eq!(meta.description.as_deref(), Some("A custom VNI variant"));
    }

    #[test]
    fn missing_required_field_json_reports_field() {
        // `name` is omitted.
        let json = r#"{ "id": "x", "rules": [] }"#;
        let err = ConfigParser::parse(json, ConfigFormat::Json).unwrap_err();
        match err {
            ParseError::MissingField { field } => assert_eq!(field, "name"),
            other => panic!("expected MissingField, got {:?}", other),
        }
    }

    #[test]
    fn missing_required_field_toml_reports_field() {
        // `rules` is omitted.
        let toml_str = "id = \"x\"\nname = \"X\"\n";
        let err = ConfigParser::parse(toml_str, ConfigFormat::Toml).unwrap_err();
        match err {
            ParseError::MissingField { field } => assert_eq!(field, "rules"),
            other => panic!("expected MissingField, got {:?}", other),
        }
    }

    #[test]
    fn syntax_error_reports_line_number() {
        // Malformed JSON: trailing comma / unterminated object across lines.
        let json = "{\n  \"id\": \"x\",\n  \"name\": \"X\"\n  \"rules\": []\n}";
        let err = ConfigParser::parse(json, ConfigFormat::Json).unwrap_err();
        match err {
            ParseError::SyntaxError { line, .. } => {
                assert!(line >= 1, "line number should be reported, got {}", line)
            }
            other => panic!("expected SyntaxError, got {:?}", other),
        }
    }

    #[test]
    fn rejects_file_exceeding_max_size() {
        // Build an input just over the 10 MB limit cheaply.
        let big = "a".repeat(MAX_CONFIG_FILE_SIZE + 1);
        let err = ConfigParser::parse(&big, ConfigFormat::Json).unwrap_err();
        match err {
            ParseError::FileTooLarge { size, max } => {
                assert_eq!(size, MAX_CONFIG_FILE_SIZE + 1);
                assert_eq!(max, MAX_CONFIG_FILE_SIZE);
            }
            other => panic!("expected FileTooLarge, got {:?}", other),
        }
    }

    #[test]
    fn line_from_byte_offset_counts_newlines() {
        let text = "a\nb\nc";
        assert_eq!(line_from_byte_offset(text, 0), 1);
        assert_eq!(line_from_byte_offset(text, 2), 2);
        assert_eq!(line_from_byte_offset(text, 4), 3);
        // Offset beyond end clamps to last line.
        assert_eq!(line_from_byte_offset(text, 999), 3);
    }

    /// Build a representative config exercising all optional fields and a
    /// rule with a full context (both `preceding` and `not_preceding`).
    fn sample_config() -> InputMethodConfig {
        InputMethodConfig {
            id: "custom-telex".to_string(),
            name: "Custom Telex".to_string(),
            rules: vec![
                TransformationRule {
                    input_sequence: "aa".to_string(),
                    output_char: 'â',
                    context: None,
                },
                TransformationRule {
                    input_sequence: "aw".to_string(),
                    output_char: 'ă',
                    context: Some(RuleContext {
                        preceding: Some(vec!['a', 'o']),
                        not_preceding: Some(vec!['x']),
                    }),
                },
            ],
            metadata: Some(ConfigMetadata {
                version: "1.0".to_string(),
                author: Some("user".to_string()),
                description: Some("A custom Telex variant".to_string()),
            }),
        }
    }

    #[test]
    fn json_round_trip_preserves_config() {
        let config = sample_config();
        let json = ConfigPrettyPrinter::format(&config, ConfigFormat::Json);
        let reparsed = ConfigParser::parse(&json, ConfigFormat::Json)
            .expect("pretty-printed JSON should re-parse");
        assert_eq!(reparsed, config);
    }

    #[test]
    fn toml_round_trip_preserves_config() {
        let config = sample_config();
        let toml_str = ConfigPrettyPrinter::format(&config, ConfigFormat::Toml);
        let reparsed = ConfigParser::parse(&toml_str, ConfigFormat::Toml)
            .expect("pretty-printed TOML should re-parse");
        assert_eq!(reparsed, config);
    }

    #[test]
    fn round_trip_without_optional_fields() {
        // Config with no metadata and rules without context: optional fields
        // must be omitted (not emitted as null) so TOML stays valid.
        let config = InputMethodConfig {
            id: "minimal".to_string(),
            name: "Minimal".to_string(),
            rules: vec![TransformationRule {
                input_sequence: "dd".to_string(),
                output_char: 'đ',
                context: None,
            }],
            metadata: None,
        };

        let json = ConfigPrettyPrinter::format(&config, ConfigFormat::Json);
        assert!(
            !json.contains("null"),
            "JSON output should omit None fields, got: {}",
            json
        );
        let from_json = ConfigParser::parse(&json, ConfigFormat::Json)
            .expect("minimal JSON should re-parse");
        assert_eq!(from_json, config);

        let toml_str = ConfigPrettyPrinter::format(&config, ConfigFormat::Toml);
        let from_toml = ConfigParser::parse(&toml_str, ConfigFormat::Toml)
            .expect("minimal TOML should re-parse");
        assert_eq!(from_toml, config);
    }

    #[test]
    fn formatted_output_parses_back_for_existing_samples() {
        // The parsed sample fixtures should survive a format -> parse cycle.
        let json_config = ConfigParser::parse(VALID_JSON, ConfigFormat::Json).unwrap();
        let json = ConfigPrettyPrinter::format(&json_config, ConfigFormat::Json);
        assert_eq!(
            ConfigParser::parse(&json, ConfigFormat::Json).unwrap(),
            json_config
        );

        let toml_config = ConfigParser::parse(VALID_TOML, ConfigFormat::Toml).unwrap();
        let toml_str = ConfigPrettyPrinter::format(&toml_config, ConfigFormat::Toml);
        assert_eq!(
            ConfigParser::parse(&toml_str, ConfigFormat::Toml).unwrap(),
            toml_config
        );
    }

    // ---- Rule validation: helpers ----------------------------------------

    /// Build a rule with no context.
    fn rule(input: &str, output: char) -> TransformationRule {
        TransformationRule {
            input_sequence: input.to_string(),
            output_char: output,
            context: None,
        }
    }

    /// Build a rule with a `preceding` context constraint.
    fn rule_with_preceding(input: &str, output: char, preceding: Vec<char>) -> TransformationRule {
        TransformationRule {
            input_sequence: input.to_string(),
            output_char: output,
            context: Some(RuleContext {
                preceding: Some(preceding),
                not_preceding: None,
            }),
        }
    }

    fn config_with_rules(rules: Vec<TransformationRule>) -> InputMethodConfig {
        InputMethodConfig {
            id: "test".to_string(),
            name: "Test".to_string(),
            rules,
            metadata: None,
        }
    }

    // ---- Rule validation: conflict detection -----------------------------

    #[test]
    fn valid_config_without_conflicts_or_cycles_passes() {
        let config = config_with_rules(vec![
            rule("aa", 'â'),
            rule("aw", 'ă'),
            rule("ee", 'ê'),
            rule("dd", 'đ'),
        ]);
        assert_eq!(ConfigParser::validate_rules(&config), Ok(()));
    }

    #[test]
    fn duplicate_rule_same_output_is_not_a_conflict() {
        // Same input AND same output appearing twice is redundant, not a
        // conflict (only one distinct output character).
        let config = config_with_rules(vec![rule("aa", 'â'), rule("aa", 'â')]);
        assert_eq!(ConfigParser::validate_rules(&config), Ok(()));
    }

    #[test]
    fn conflicting_mapping_same_input_different_output_detected() {
        let config = config_with_rules(vec![rule("aa", 'â'), rule("aa", 'ä')]);
        let errors = ConfigParser::validate_rules(&config).unwrap_err();
        assert_eq!(errors.len(), 1);
        match &errors[0] {
            ValidationError::ConflictingMapping { input, outputs } => {
                assert_eq!(input, "aa");
                assert_eq!(outputs, &vec!['â', 'ä']);
            }
            other => panic!("expected ConflictingMapping, got {:?}", other),
        }
    }

    #[test]
    fn multiple_conflicts_are_all_collected() {
        let config = config_with_rules(vec![
            rule("aa", 'â'),
            rule("aa", 'ä'),
            rule("ee", 'ê'),
            rule("ee", 'ë'),
        ]);
        let errors = ConfigParser::validate_rules(&config).unwrap_err();
        let conflicts: Vec<_> = errors
            .iter()
            .filter(|e| matches!(e, ValidationError::ConflictingMapping { .. }))
            .collect();
        assert_eq!(conflicts.len(), 2);
        // Reported in first-seen input order: "aa" then "ee".
        match conflicts[0] {
            ValidationError::ConflictingMapping { input, .. } => assert_eq!(input, "aa"),
            _ => unreachable!(),
        }
        match conflicts[1] {
            ValidationError::ConflictingMapping { input, .. } => assert_eq!(input, "ee"),
            _ => unreachable!(),
        }
    }

    #[test]
    fn same_input_different_context_is_not_a_conflict() {
        // Two rules share the input "w" but apply under different preceding
        // contexts, so they never fire for the same keystrokes -> no conflict.
        let config = config_with_rules(vec![
            rule_with_preceding("w", 'ơ', vec!['o']),
            rule_with_preceding("w", 'ư', vec!['u']),
        ]);
        assert_eq!(ConfigParser::validate_rules(&config), Ok(()));
    }

    #[test]
    fn three_distinct_outputs_reported_in_first_seen_order() {
        let config = config_with_rules(vec![
            rule("x", 'a'),
            rule("x", 'b'),
            rule("x", 'c'),
        ]);
        let errors = ConfigParser::validate_rules(&config).unwrap_err();
        let conflict = errors
            .iter()
            .find_map(|e| match e {
                ValidationError::ConflictingMapping { input, outputs } if input == "x" => {
                    Some(outputs.clone())
                }
                _ => None,
            })
            .expect("conflict for input x");
        assert_eq!(conflict, vec!['a', 'b', 'c']);
    }

    // ---- Rule validation: cycle detection --------------------------------

    #[test]
    fn acyclic_ruleset_passes_cycle_check() {
        // a -> b -> c, no back edge.
        let config = config_with_rules(vec![rule("a", 'b'), rule("b", 'c'), rule("c", 'd')]);
        assert_eq!(ConfigParser::validate_rules(&config), Ok(()));
    }

    #[test]
    fn direct_two_rule_cycle_detected() {
        // a -> b and b -> a form a cycle through both rules.
        let config = config_with_rules(vec![rule("a", 'b'), rule("b", 'a')]);
        let errors = ConfigParser::validate_rules(&config).unwrap_err();
        let cycles: Vec<_> = errors
            .iter()
            .filter_map(|e| match e {
                ValidationError::CyclicRule { rules } => Some(rules.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(cycles.len(), 1, "exactly one cycle expected: {:?}", errors);
        let mut involved = cycles[0].clone();
        involved.sort_unstable();
        assert_eq!(involved, vec![0, 1]);
    }

    #[test]
    fn self_loop_cycle_detected() {
        // A rule whose output feeds its own input: "a" -> 'a'.
        let config = config_with_rules(vec![rule("a", 'a')]);
        let errors = ConfigParser::validate_rules(&config).unwrap_err();
        let cycles: Vec<_> = errors
            .iter()
            .filter(|e| matches!(e, ValidationError::CyclicRule { .. }))
            .collect();
        assert_eq!(cycles.len(), 1);
        match cycles[0] {
            ValidationError::CyclicRule { rules } => assert_eq!(rules, &vec![0]),
            _ => unreachable!(),
        }
    }

    #[test]
    fn longer_cycle_detected() {
        // a -> b -> c -> a (three-rule cycle).
        let config = config_with_rules(vec![rule("a", 'b'), rule("b", 'c'), rule("c", 'a')]);
        let errors = ConfigParser::validate_rules(&config).unwrap_err();
        let cycles: Vec<_> = errors
            .iter()
            .filter_map(|e| match e {
                ValidationError::CyclicRule { rules } => Some(rules.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(cycles.len(), 1);
        let mut involved = cycles[0].clone();
        involved.sort_unstable();
        assert_eq!(involved, vec![0, 1, 2]);
    }

    #[test]
    fn conflicts_and_cycles_collected_together() {
        // One conflict ("aa" -> â/ä) and one cycle (a -> b -> a).
        let config = config_with_rules(vec![
            rule("aa", 'â'),
            rule("aa", 'ä'),
            rule("a", 'b'),
            rule("b", 'a'),
        ]);
        let errors = ConfigParser::validate_rules(&config).unwrap_err();
        let has_conflict = errors
            .iter()
            .any(|e| matches!(e, ValidationError::ConflictingMapping { .. }));
        let has_cycle = errors
            .iter()
            .any(|e| matches!(e, ValidationError::CyclicRule { .. }));
        assert!(has_conflict, "expected a conflict in {:?}", errors);
        assert!(has_cycle, "expected a cycle in {:?}", errors);
    }

    #[test]
    fn empty_ruleset_passes_validation() {
        let config = config_with_rules(vec![]);
        assert_eq!(ConfigParser::validate_rules(&config), Ok(()));
    }

    // ---- Additional config-parser edge cases (Task 14.7) -----------------

    #[test]
    fn invalid_toml_syntax_reports_line_number() {
        // An unterminated string on line 3 is a TOML syntax error.
        let toml_str = "id = \"x\"\nname = \"X\"\nrules = [oops\n";
        let err = ConfigParser::parse(toml_str, ConfigFormat::Toml).unwrap_err();
        match err {
            ParseError::SyntaxError { line, .. } => {
                assert!(line >= 1, "line number should be reported, got {}", line);
            }
            other => panic!("expected SyntaxError, got {:?}", other),
        }
    }

    #[test]
    fn empty_input_is_rejected() {
        // Empty documents are not valid configurations in either format.
        assert!(ConfigParser::parse("", ConfigFormat::Json).is_err());
        // An empty TOML document parses as an empty table, which is missing
        // the required `id` field.
        match ConfigParser::parse("", ConfigFormat::Toml).unwrap_err() {
            ParseError::MissingField { .. } | ParseError::SyntaxError { .. } => {}
            other => panic!("expected MissingField or SyntaxError, got {:?}", other),
        }
    }

    #[test]
    fn input_at_exact_max_size_is_not_rejected_for_size() {
        // The size guard rejects strictly greater than the maximum, so an
        // input of exactly MAX bytes must fail for *content* reasons (it is
        // not valid JSON) rather than as FileTooLarge.
        let exact = "a".repeat(MAX_CONFIG_FILE_SIZE);
        let err = ConfigParser::parse(&exact, ConfigFormat::Json).unwrap_err();
        assert!(
            !matches!(err, ParseError::FileTooLarge { .. }),
            "input of exactly MAX bytes must not be rejected as too large, got {:?}",
            err
        );
    }

    #[test]
    fn invalid_output_char_value_is_rejected() {
        // `output_char` must be a single character; a multi-character string
        // is a data error, surfaced as a parse failure rather than a panic.
        let json = r#"{ "id": "x", "name": "X",
            "rules": [{ "input_sequence": "aa", "output_char": "ab" }] }"#;
        assert!(ConfigParser::parse(json, ConfigFormat::Json).is_err());
    }

    // ---- Property-based generators (deterministic, dependency-free) ------
    //
    // The two properties below need many random-but-valid `InputMethodConfig`
    // values. To avoid adding a property-testing dependency, a small inline
    // linear-congruential PRNG drives hand-written generators. Seeds are fixed
    // so failures are reproducible.

    /// A tiny deterministic PRNG (LCG core + xorshift output mixing).
    struct Lcg {
        state: u64,
    }

    impl Lcg {
        fn new(seed: u64) -> Self {
            Lcg { state: seed }
        }

        fn next_u64(&mut self) -> u64 {
            // LCG step with well-known 64-bit constants.
            self.state = self
                .state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            // Mix the output so low bits are usable.
            let mut x = self.state;
            x ^= x >> 33;
            x = x.wrapping_mul(0xff51afd7ed558ccd);
            x ^= x >> 33;
            x
        }

        /// Uniform-ish value in `0..n` (n must be > 0).
        fn below(&mut self, n: usize) -> usize {
            (self.next_u64() % n as u64) as usize
        }

        /// Inclusive range `lo..=hi` (requires `lo <= hi`).
        fn range(&mut self, lo: usize, hi: usize) -> usize {
            lo + self.below(hi - lo + 1)
        }

        fn flip(&mut self) -> bool {
            self.next_u64() & 1 == 1
        }
    }

    /// Distinct output characters (mix of ASCII and Vietnamese, all unique).
    const OUTPUT_CHARS: &[char] = &[
        'a', 'â', 'ă', 'e', 'ê', 'o', 'ô', 'ơ', 'u', 'ư', 'i', 'y', 'đ', 'á', 'à', 'ả', 'ã', 'ạ',
        'Đ', 'Ê',
    ];

    /// Characters used to build input sequences (kept to plain ASCII letters).
    const SEQ_CHARS: &[char] = &['a', 'b', 'c', 'd', 'e', 'o', 'u', 'w', 's', 'f', 'r', 'x', 'j'];

    fn gen_sequence(rng: &mut Lcg) -> String {
        let len = rng.range(1, 4);
        (0..len).map(|_| SEQ_CHARS[rng.below(SEQ_CHARS.len())]).collect()
    }

    fn gen_char_vec(rng: &mut Lcg) -> Vec<char> {
        let len = rng.range(1, 3);
        (0..len)
            .map(|_| OUTPUT_CHARS[rng.below(OUTPUT_CHARS.len())])
            .collect()
    }

    fn gen_context(rng: &mut Lcg) -> Option<RuleContext> {
        if !rng.flip() {
            return None;
        }
        let preceding = if rng.flip() { Some(gen_char_vec(rng)) } else { None };
        let not_preceding = if rng.flip() { Some(gen_char_vec(rng)) } else { None };
        if preceding.is_none() && not_preceding.is_none() {
            return None;
        }
        Some(RuleContext {
            preceding,
            not_preceding,
        })
    }

    fn gen_metadata(rng: &mut Lcg) -> Option<ConfigMetadata> {
        if !rng.flip() {
            return None;
        }
        Some(ConfigMetadata {
            version: format!("{}.{}", rng.range(0, 9), rng.range(0, 9)),
            author: if rng.flip() {
                Some(format!("author{}", rng.range(0, 99)))
            } else {
                None
            },
            description: if rng.flip() {
                Some(format!("desc {}", rng.range(0, 99)))
            } else {
                None
            },
        })
    }

    fn gen_config(rng: &mut Lcg, i: usize) -> InputMethodConfig {
        let rule_count = rng.range(1, 6);
        let rules = (0..rule_count)
            .map(|_| TransformationRule {
                input_sequence: gen_sequence(rng),
                output_char: OUTPUT_CHARS[rng.below(OUTPUT_CHARS.len())],
                context: gen_context(rng),
            })
            .collect();
        InputMethodConfig {
            id: format!("method-{}", i),
            name: format!("Method {}", i),
            rules,
            metadata: gen_metadata(rng),
        }
    }

    // Feature: vietnamese-input-support, Property 10: Configuration Round-Trip
    //
    // **Property 10: Configuration Round-Trip**
    // For any valid InputMethodConfig, parse(format(config)) == config
    // (structurally identical) for BOTH JSON and TOML.
    // **Validates: Requirements 16.5**
    #[test]
    fn property_10_configuration_round_trip() {
        let mut rng = Lcg::new(0x1234_5678_9abc_def0);
        for i in 0..150 {
            let config = gen_config(&mut rng, i);

            // JSON round-trip.
            let json = ConfigPrettyPrinter::format(&config, ConfigFormat::Json);
            let from_json = ConfigParser::parse(&json, ConfigFormat::Json)
                .unwrap_or_else(|e| panic!("config {} JSON re-parse failed: {}\n{}", i, e, json));
            assert_eq!(from_json, config, "JSON round-trip mismatch for config {}", i);

            // TOML round-trip.
            let toml_str = ConfigPrettyPrinter::format(&config, ConfigFormat::Toml);
            let from_toml = ConfigParser::parse(&toml_str, ConfigFormat::Toml).unwrap_or_else(|e| {
                panic!("config {} TOML re-parse failed: {}\n{}", i, e, toml_str)
            });
            assert_eq!(from_toml, config, "TOML round-trip mismatch for config {}", i);
        }
    }

    // Feature: vietnamese-input-support, Property 11: Configuration Conflict Detection
    //
    // **Property 11: Configuration Conflict Detection**
    // For rule sets with injected conflicting mappings (same input_sequence +
    // same context -> different output_char), validate_rules detects and
    // reports ALL conflicts.
    // **Validates: Requirements 16.6, 16.7**
    #[test]
    fn property_11_configuration_conflict_detection() {
        let mut rng = Lcg::new(0x0fed_cba9_8765_4321);
        for iter in 0..120 {
            // Base rules: each has a unique input sequence (and no context), so
            // they never conflict among themselves and form no cycles (their
            // multi-character inputs never equal a single-character output).
            let base_count = rng.range(2, 8);
            let base_rules: Vec<TransformationRule> = (0..base_count)
                .map(|j| TransformationRule {
                    input_sequence: format!("k{}_{}", iter, j),
                    output_char: OUTPUT_CHARS[rng.below(OUTPUT_CHARS.len())],
                    context: None,
                })
                .collect();

            // Inject conflicts: re-use an existing input sequence with a
            // guaranteed-different output character.
            let mut rules = base_rules.clone();
            let mut expected: std::collections::BTreeSet<String> =
                std::collections::BTreeSet::new();
            let conflict_count = rng.range(1, base_count);
            for _ in 0..conflict_count {
                let idx = rng.below(base_count);
                let base = &base_rules[idx];
                // Pick an output distinct from the base rule's output. Since
                // OUTPUT_CHARS holds distinct values, advancing to the next
                // index always yields a different character.
                let mut k = rng.below(OUTPUT_CHARS.len());
                if OUTPUT_CHARS[k] == base.output_char {
                    k = (k + 1) % OUTPUT_CHARS.len();
                }
                rules.push(TransformationRule {
                    input_sequence: base.input_sequence.clone(),
                    output_char: OUTPUT_CHARS[k],
                    context: None,
                });
                expected.insert(base.input_sequence.clone());
            }

            let config = config_with_rules(rules);
            let errors = ConfigParser::validate_rules(&config)
                .expect_err("injected conflicts must fail validation");

            // Every injected conflict must be reported, and no spurious ones.
            let reported: std::collections::BTreeSet<String> = errors
                .iter()
                .filter_map(|e| match e {
                    ValidationError::ConflictingMapping { input, .. } => Some(input.clone()),
                    _ => None,
                })
                .collect();
            assert_eq!(
                reported, expected,
                "iteration {}: reported conflict set does not match injected set",
                iter
            );
        }
    }
}
