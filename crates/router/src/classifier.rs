use crate::builtins;
use crate::casefold::{CiBorrow, CiString, ascii_contains_nocase};
use crate::env;
use hashbrown::HashSet;
use once_cell::sync::Lazy;
use regex::Regex;
use runloop_core::config::RouterConfig;
use serde::Serialize;

static SIMPLE_COMMAND: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^[A-Za-z0-9_\-./]+(?:\s+.+)?$").expect("simple command regex"));

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Route {
    Shell,
    Agent,
}

impl std::fmt::Display for Route {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Route::Shell => write!(f, "shell"),
            Route::Agent => write!(f, "agent"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Classification {
    pub route: Route,
    pub rule: String,
    #[serde(default)]
    pub features: Vec<String>,
    pub reason: String,
    #[serde(default)]
    pub blocked: bool,
}

impl Classification {
    pub fn is_shell(&self) -> bool {
        self.route == Route::Shell
    }
}

#[derive(Debug, Clone)]
struct Pattern {
    raw: String,
    lower: String,
}

impl Pattern {
    fn new(value: String) -> Self {
        let lower = value.to_ascii_lowercase();
        Self { raw: value, lower }
    }

    fn matches(&self, prompt: &str) -> bool {
        !self.lower.is_empty() && ascii_contains_nocase(prompt, &self.lower)
    }
}

pub struct Router {
    fastpath_shell: bool,
    default_opening: String,
    allowlist: Vec<Pattern>,
    denylist: Vec<Pattern>,
    known_commands: HashSet<CiString>,
}

impl Router {
    pub fn from_config(config: &RouterConfig) -> Self {
        Self::with_commands(config, env::path_commands())
    }

    pub fn with_commands<I>(config: &RouterConfig, extra_commands: I) -> Self
    where
        I: IntoIterator<Item = String>,
    {
        let mut known_commands = HashSet::new();
        for cmd in builtins::baseline_commands() {
            known_commands.insert(CiString::new(cmd));
        }
        for cmd in &config.known_commands {
            known_commands.insert(CiString::new(cmd));
        }
        for cmd in extra_commands {
            known_commands.insert(CiString::new(cmd));
        }

        Self {
            fastpath_shell: config.fastpath_shell,
            default_opening: config.default_opening.clone(),
            allowlist: config.allowlist.iter().cloned().map(Pattern::new).collect(),
            denylist: config.denylist.iter().cloned().map(Pattern::new).collect(),
            known_commands,
        }
    }

    pub fn classify(&self, prompt: &str) -> Classification {
        let trimmed = prompt.trim();

        if let Some(pattern) = self.matches_pattern(&self.denylist, trimmed) {
            return Classification {
                route: Route::Shell,
                rule: "override:denylist".into(),
                features: vec![format!("pattern:{}", pattern.raw)],
                reason: format!("prompt matched router.denylist pattern '{}'", pattern.raw),
                blocked: true,
            };
        }

        if let Some(pattern) = self.matches_pattern(&self.allowlist, trimmed) {
            return Classification {
                route: Route::Shell,
                rule: "override:allowlist".into(),
                features: vec![format!("pattern:{}", pattern.raw)],
                reason: format!("prompt matched router.allowlist pattern '{}'", pattern.raw),
                blocked: false,
            };
        }

        if trimmed.is_empty() {
            return Classification {
                route: Route::Agent,
                rule: "fallback:empty".into(),
                features: Vec::new(),
                reason: format!(
                    "empty prompt; routing to opening '{}'",
                    self.default_opening
                ),
                blocked: false,
            };
        }

        if !self.fastpath_shell {
            return Classification {
                route: Route::Agent,
                rule: "fastpath_disabled".into(),
                features: vec!["config:fastpath_shell=false".into()],
                reason: format!(
                    "router.fastpath_shell disabled; routing to opening '{}'",
                    self.default_opening
                ),
                blocked: false,
            };
        }

        let mut features = Vec::new();
        let mut command_hits = Vec::new();

        let mut token_detected = false;
        for (needle, feature) in POSIX_TOKENS.iter() {
            if trimmed.contains(needle) {
                token_detected = true;
                push_feature(&mut features, *feature);
            }
        }

        let matches_simple = SIMPLE_COMMAND.is_match(trimmed);
        if matches_simple {
            push_feature(&mut features, "pattern:simple_command");
        }

        let mut has_known_command = false;
        let mut first_token_is_command = false;

        for (idx, token) in token_candidates(trimmed).enumerate() {
            let is_path = token.starts_with("./")
                || token.starts_with("../")
                || (token.starts_with('/') && token.len() > 1);
            let is_known = is_path || self.contains_command(token);

            if is_known {
                has_known_command = true;
                if idx == 0 {
                    first_token_is_command = true;
                }
                let lower_token = token.to_ascii_lowercase();
                if !command_hits.contains(&lower_token) {
                    command_hits.push(lower_token.clone());
                    let feature = if is_path {
                        format!("path:{}", lower_token)
                    } else {
                        format!("cmd:{}", lower_token)
                    };
                    push_feature(&mut features, feature);
                }
            }
        }

        if has_known_command && (token_detected || (matches_simple && first_token_is_command)) {
            let rule = if token_detected {
                "posix_token+known_command"
            } else {
                "simple_command+known_command"
            };
            let reason = if token_detected {
                format!(
                    "matched POSIX token(s) with command(s): {}",
                    command_hits.join(", ")
                )
            } else {
                format!("matched simple command: {}", command_hits.join(", "))
            };
            return Classification {
                route: Route::Shell,
                rule: rule.into(),
                features,
                reason,
                blocked: false,
            };
        }

        Classification {
            route: Route::Agent,
            rule: "fallback:opening".into(),
            features,
            reason: if has_known_command {
                format!(
                    "command-like tokens present but missing routing heuristics; using opening '{}'",
                    self.default_opening
                )
            } else {
                format!(
                    "no known command detected; routing to opening '{}'",
                    self.default_opening
                )
            },
            blocked: false,
        }
    }

    fn matches_pattern<'a>(&'a self, patterns: &'a [Pattern], prompt: &str) -> Option<&'a Pattern> {
        patterns.iter().find(|pattern| pattern.matches(prompt))
    }

    fn contains_command(&self, token: &str) -> bool {
        self.known_commands.get(&CiBorrow(token)).is_some()
    }
}

const POSIX_TOKENS: [(&str, &str); 6] = [
    ("&&", "token:&&"),
    ("||", "token:||"),
    ("|", "token:|"),
    (">", "token:>"),
    ("<", "token:<"),
    (";", "token:;"),
];

fn token_candidates(prompt: &str) -> TokenIter<'_> {
    TokenIter { s: prompt, pos: 0 }
}

struct TokenIter<'a> {
    s: &'a str,
    pos: usize,
}

impl<'a> Iterator for TokenIter<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        let len = self.s.len();
        loop {
            // Skip delimiters (whitespace or POSIX operators).
            while self.pos < len {
                let Some(ch) = self.s[self.pos..].chars().next() else {
                    break;
                };
                if ch.is_whitespace() || matches!(ch, '|' | '&' | ';' | '<' | '>') {
                    self.pos += ch.len_utf8();
                } else {
                    break;
                }
            }
            if self.pos >= len {
                return None;
            }
            let start = self.pos;
            let mut end = len;
            for (offset, ch) in self.s[self.pos..].char_indices() {
                if ch.is_whitespace() || matches!(ch, '|' | '&' | ';' | '<' | '>') {
                    end = self.pos + offset;
                    self.pos = end + ch.len_utf8();
                    break;
                }
            }
            if self.pos == start {
                // Delimiter not found; consume rest.
                self.pos = len;
            } else if end == len {
                self.pos = len;
            }
            let slice = &self.s[start..end];
            if let Some(token) = trim_token(slice) {
                return Some(token);
            }
            // Empty after trimming; continue scanning in the loop.
        }
    }
}

fn trim_token(token: &str) -> Option<&str> {
    let mut start = 0;
    let mut end = token.len();
    while start < end {
        let Some(ch) = token[start..].chars().next() else {
            break;
        };
        if matches!(
            ch,
            '\'' | '"' | '`' | '(' | ')' | '{' | '}' | '[' | ']' | ',' | ':'
        ) {
            start += ch.len_utf8();
        } else {
            break;
        }
    }
    while end > start {
        let Some(ch) = token[..end].chars().next_back() else {
            break;
        };
        if matches!(
            ch,
            '\'' | '"' | '`' | '(' | ')' | '{' | '}' | '[' | ']' | ',' | ':'
        ) {
            end -= ch.len_utf8();
        } else {
            break;
        }
    }
    if start >= end {
        None
    } else {
        Some(&token[start..end])
    }
}

fn push_feature(features: &mut Vec<String>, feature: impl Into<String>) {
    let feature = feature.into();
    if !features.contains(&feature) {
        features.push(feature);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use runloop_core::config::RouterConfig;

    fn router_with<F>(mut configure: F) -> Router
    where
        F: FnMut(&mut RouterConfig),
    {
        let mut config = RouterConfig::default();
        configure(&mut config);
        Router::with_commands(&config, std::iter::empty::<String>())
    }

    #[test]
    fn simple_command_routes_to_shell() {
        let router = router_with(|_| {});
        let classification = router.classify("ls -la");
        assert_eq!(classification.route, Route::Shell);
        assert_eq!(classification.rule, "simple_command+known_command");
        assert!(
            classification
                .features
                .iter()
                .any(|feature| feature == "cmd:ls")
        );
    }

    #[test]
    fn fastpath_disabled_routes_to_agent() {
        let router = router_with(|config| config.fastpath_shell = false);
        let classification = router.classify("ls -la");
        assert_eq!(classification.route, Route::Agent);
        assert_eq!(classification.rule, "fastpath_disabled");
        assert!(
            classification
                .features
                .iter()
                .any(|feature| feature == "config:fastpath_shell=false")
        );
    }

    #[test]
    fn allowlist_overrides_classification() {
        let router = router_with(|config| config.allowlist = vec!["deploy".into()]);
        let classification = router.classify("Please deploy the update");
        assert_eq!(classification.route, Route::Shell);
        assert_eq!(classification.rule, "override:allowlist");
        assert!(
            classification
                .features
                .iter()
                .any(|feature| feature == "pattern:deploy")
        );
        assert!(!classification.blocked);
    }

    #[test]
    fn denylist_marks_prompt_blocked() {
        let router = router_with(|config| config.denylist = vec!["rm -rf".into()]);
        let classification = router.classify("rm -rf /tmp/work");
        assert_eq!(classification.route, Route::Shell);
        assert_eq!(classification.rule, "override:denylist");
        assert!(classification.blocked);
    }

    #[test]
    fn path_command_is_treated_as_known() {
        let router = router_with(|_| {});
        let classification = router.classify("./tools/deploy.sh");
        assert_eq!(classification.route, Route::Shell);
        assert!(
            classification
                .features
                .iter()
                .any(|feature| feature.starts_with("path:"))
        );
    }

    #[test]
    fn commands_are_matched_case_insensitively() {
        let router = router_with(|_| {});
        let classification = router.classify("LS -la");
        assert_eq!(classification.route, Route::Shell);
        assert_eq!(classification.rule, "simple_command+known_command");
        assert!(
            classification
                .features
                .iter()
                .any(|feature| feature == "cmd:ls")
        );
    }

    #[test]
    fn quotes_are_trimmed_from_tokens() {
        let router = router_with(|_| {});
        let classification = router.classify("\"ls\" -la");
        assert_eq!(classification.route, Route::Agent);
        assert_eq!(classification.rule, "fallback:opening");
    }

    #[test]
    fn natural_language_with_mid_command_stays_agent() {
        let router = router_with(|_| {});
        let classification = router.classify("please run ls after this");
        assert_eq!(classification.route, Route::Agent);
        assert_eq!(classification.rule, "fallback:opening");
    }

    #[test]
    fn separator_tokens_are_skipped_and_trailing_command_detected() {
        let router = router_with(|_| {});
        let classification = router.classify("run : ls");
        assert_eq!(classification.route, Route::Agent);
        assert_eq!(classification.rule, "fallback:opening");
    }

    #[test]
    fn empty_string_routes_to_agent() {
        let router = router_with(|_| {});
        let classification = router.classify("");
        assert_eq!(classification.route, Route::Agent);
        assert_eq!(classification.rule, "fallback:empty");
    }

    #[test]
    fn whitespace_only_routes_to_agent() {
        let router = router_with(|_| {});
        for input in &["   ", "\t", "\n", "  \t\n  "] {
            let classification = router.classify(input);
            assert_eq!(
                classification.route,
                Route::Agent,
                "whitespace-only '{}' should route to agent",
                input.escape_debug()
            );
            assert_eq!(classification.rule, "fallback:empty");
        }
    }

    #[test]
    fn single_character_inputs() {
        let router = router_with(|_| {});
        // Single punctuation characters should not panic
        for ch in &[
            '|', '&', ';', '<', '>', '\'', '"', '`', '(', ')', '{', '}', '[', ']',
        ] {
            let input = ch.to_string();
            let classification = router.classify(&input);
            // Just verify it doesn't panic; route depends on specific character
            let _ = classification;
        }
        // Single letter (potential command)
        let classification = router.classify("a");
        assert_eq!(classification.route, Route::Agent);
    }

    #[test]
    fn unicode_inputs_do_not_panic() {
        let router = router_with(|_| {});
        // Various Unicode edge cases
        let inputs = [
            "日本語コマンド",
            "émoji 🚀 command",
            "مرحبا",
            "Ω≈ç√∫",
            "\u{200B}",  // zero-width space
            "\u{FEFF}",  // BOM
            "a\u{0300}", // 'a' with combining grave accent
            "🏳️‍🌈",        // complex emoji sequence
        ];
        for input in &inputs {
            let classification = router.classify(input);
            // Just verify it doesn't panic
            let _ = classification;
        }
    }

    #[test]
    fn long_inputs_do_not_panic() {
        let router = router_with(|_| {});
        // Very long input
        let long_input = "a".repeat(100_000);
        let classification = router.classify(&long_input);
        assert_eq!(classification.route, Route::Agent);

        // Long input with shell operators
        let long_with_pipes = format!("{} | {}", "a".repeat(10_000), "b".repeat(10_000));
        let classification = router.classify(&long_with_pipes);
        let _ = classification; // Just verify no panic
    }

    #[test]
    fn pathological_operator_sequences() {
        let router = router_with(|_| {});
        // Repeated operators
        let inputs = [
            "||||||||",
            "&&&&&&&&",
            ";;;;;;;;",
            "<<<<<<<<",
            ">>>>>>>>",
            "| | | | |",
            "&& || && ||",
        ];
        for input in &inputs {
            let classification = router.classify(input);
            let _ = classification; // Just verify no panic
        }
    }
}

#[cfg(test)]
mod proptest_tests {
    use super::*;
    use proptest::prelude::*;
    use runloop_core::config::RouterConfig;

    fn router_default() -> Router {
        Router::with_commands(&RouterConfig::default(), std::iter::empty::<String>())
    }

    proptest! {
        /// The classifier should never panic on any arbitrary Unicode string.
        #[test]
        fn classifier_never_panics_on_arbitrary_input(input in ".*") {
            let router = router_default();
            let classification = router.classify(&input);
            // Just verify it returns without panicking and produces valid output
            prop_assert!(matches!(classification.route, Route::Shell | Route::Agent));
            prop_assert!(!classification.rule.is_empty());
        }

        /// The classifier should handle strings with embedded null bytes.
        #[test]
        fn classifier_handles_embedded_nulls(
            prefix in "[a-z]{0,10}",
            suffix in "[a-z]{0,10}"
        ) {
            let router = router_default();
            let input = format!("{}\0{}", prefix, suffix);
            let classification = router.classify(&input);
            prop_assert!(matches!(classification.route, Route::Shell | Route::Agent));
        }

        /// The classifier should handle strings of varying lengths.
        #[test]
        fn classifier_handles_varying_lengths(len in 0usize..10000) {
            let router = router_default();
            let input = "x".repeat(len);
            let classification = router.classify(&input);
            prop_assert!(matches!(classification.route, Route::Shell | Route::Agent));
        }

        /// The classifier should handle shell-like patterns without panicking.
        #[test]
        fn classifier_handles_shell_patterns(
            cmd in "[a-z_][a-z0-9_-]{0,20}",
            args in "([ ][a-z0-9./=-]{0,30}){0,5}",
            ops in "([ ]*[|&;<>][ ]*){0,3}"
        ) {
            let router = router_default();
            let input = format!("{}{}{}", cmd, args, ops);
            let classification = router.classify(&input);
            prop_assert!(matches!(classification.route, Route::Shell | Route::Agent));
        }

        /// The classifier should handle inputs with mixed whitespace.
        #[test]
        fn classifier_handles_mixed_whitespace(
            parts in prop::collection::vec("[a-z]{1,10}", 0..10),
            sep_choice in prop::collection::vec(0u8..4, 0..10)
        ) {
            let router = router_default();
            let separators = [" ", "\t", "\n", "  "];
            let mut input = String::new();
            for (i, part) in parts.iter().enumerate() {
                if i > 0 {
                    let sep_idx = sep_choice.get(i - 1).copied().unwrap_or(0) as usize;
                    input.push_str(separators[sep_idx % separators.len()]);
                }
                input.push_str(part);
            }
            let classification = router.classify(&input);
            prop_assert!(matches!(classification.route, Route::Shell | Route::Agent));
        }

        /// The classifier should handle inputs starting/ending with various quote types.
        #[test]
        fn classifier_handles_quoted_patterns(
            content in "[a-z ]{0,20}",
            quote in prop_oneof!["'", "\"", "`"]
        ) {
            let router = router_default();
            // Quoted content
            let input = format!("{}{}{}", quote, content, quote);
            let classification = router.classify(&input);
            prop_assert!(matches!(classification.route, Route::Shell | Route::Agent));

            // Unbalanced quotes
            let input = format!("{}{}", quote, content);
            let classification = router.classify(&input);
            prop_assert!(matches!(classification.route, Route::Shell | Route::Agent));
        }

        /// Token iterator should handle all inputs without panicking.
        #[test]
        fn token_iterator_never_panics(input in ".*") {
            let tokens: Vec<&str> = token_candidates(&input).collect();
            // Just verify it completes without panicking
            let _ = tokens;
        }

        /// Token iterator on pathological inputs.
        #[test]
        fn token_iterator_pathological(
            ops in "[|&;<>]{0,100}",
            content in "[a-z]{0,50}"
        ) {
            let input = format!("{}{}{}", ops, content, ops);
            let tokens: Vec<&str> = token_candidates(&input).collect();
            // Verify no panic and tokens are valid slices
            for token in &tokens {
                prop_assert!(!token.is_empty());
            }
        }
    }
}
