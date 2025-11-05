use crate::builtins;
use crate::env;
use once_cell::sync::Lazy;
use regex::Regex;
use runloop_core::config::RouterConfig;
use serde::Serialize;
use std::collections::HashSet;

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

    fn matches(&self, prompt_lower: &str) -> bool {
        !self.lower.is_empty() && prompt_lower.contains(&self.lower)
    }
}

pub struct Router {
    fastpath_shell: bool,
    default_opening: String,
    allowlist: Vec<Pattern>,
    denylist: Vec<Pattern>,
    known_commands: HashSet<String>,
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
            known_commands.insert(cmd.to_ascii_lowercase());
        }
        for cmd in &config.known_commands {
            known_commands.insert(cmd.to_ascii_lowercase());
        }
        for cmd in extra_commands {
            known_commands.insert(cmd.to_ascii_lowercase());
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
        let prompt_lower = trimmed.to_ascii_lowercase();

        if let Some(pattern) = self.matches_pattern(&self.denylist, &prompt_lower) {
            return Classification {
                route: Route::Shell,
                rule: "override:denylist".into(),
                features: vec![format!("pattern:{}", pattern.raw)],
                reason: format!("prompt matched router.denylist pattern '{}'", pattern.raw),
                blocked: true,
            };
        }

        if let Some(pattern) = self.matches_pattern(&self.allowlist, &prompt_lower) {
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

        let tokens: Vec<String> = token_candidates(trimmed).collect();
        let mut has_known_command = false;
        let mut first_token_is_command = false;

        for (idx, token) in tokens.iter().enumerate() {
            let is_path = token.starts_with("./")
                || token.starts_with("../")
                || (token.starts_with('/') && token.len() > 1);
            let is_known = is_path || self.known_commands.contains(token);

            if is_known {
                has_known_command = true;
                if idx == 0 {
                    first_token_is_command = true;
                }
                if !command_hits.contains(token) {
                    command_hits.push(token.clone());
                    let feature = if is_path {
                        format!("path:{}", token)
                    } else {
                        format!("cmd:{}", token)
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

    fn matches_pattern<'a>(
        &'a self,
        patterns: &'a [Pattern],
        prompt_lower: &str,
    ) -> Option<&'a Pattern> {
        patterns
            .iter()
            .find(|pattern| pattern.matches(prompt_lower))
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

fn token_candidates(prompt: &str) -> impl Iterator<Item = String> + '_ {
    prompt
        .split(|c: char| c.is_whitespace() || "|&;<>".contains(c))
        .filter_map(normalize_token)
}

fn normalize_token(raw: &str) -> Option<String> {
    let trimmed = raw.trim_matches(|c: char| {
        matches!(
            c,
            '\'' | '"' | '`' | '(' | ')' | '{' | '}' | '[' | ']' | ',' | ':'
        )
    });
    if trimmed.is_empty() {
        return None;
    }
    let token = trimmed.to_ascii_lowercase();
    if token.is_empty() { None } else { Some(token) }
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
}
