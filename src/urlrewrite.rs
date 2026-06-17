use color_eyre::eyre;
use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Rule {
    pub r#match: String,
    pub replace: String,
}

#[derive(Debug, Clone)]
pub struct Rewriter {
    rules: Vec<CompiledRule>,
}

#[derive(Debug, Clone)]
struct CompiledRule {
    regex: Regex,
    replace: String,
}

impl Rewriter {
    pub fn new(rules: &[Rule]) -> eyre::Result<Option<Self>> {
        if rules.is_empty() {
            return Ok(None);
        }

        let mut compiled = Vec::with_capacity(rules.len());
        for (index, rule) in rules.iter().enumerate() {
            let regex = Regex::new(&rule.r#match)
                .map_err(|err| eyre::eyre!("url_rewrites[{index}]: {err}"))?;
            compiled.push(CompiledRule {
                regex,
                replace: rule.replace.clone(),
            });
        }

        Ok(Some(Self { rules: compiled }))
    }

    pub fn apply(&self, raw_url: &str) -> String {
        for rule in &self.rules {
            if rule.regex.is_match(raw_url) {
                return rule
                    .regex
                    .replace_all(raw_url, rule.replace.as_str())
                    .to_string();
            }
        }
        raw_url.to_string()
    }
}

pub fn apply(rewriter: &Option<Rewriter>, raw_url: &str) -> String {
    match rewriter {
        Some(rewriter) => rewriter.apply(raw_url),
        None => raw_url.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{Rewriter, Rule, apply};

    #[test]
    fn empty_rules_have_no_rewriter() {
        assert!(Rewriter::new(&[]).unwrap().is_none());
    }

    #[test]
    fn rejects_invalid_regexes() {
        assert!(
            Rewriter::new(&[Rule {
                r#match: "[".to_string(),
                replace: "x".to_string(),
            }])
            .is_err()
        );
    }

    #[test]
    fn none_rewriter_is_identity() {
        assert_eq!(
            apply(&None, "https://example.com/x"),
            "https://example.com/x"
        );
    }

    #[test]
    fn no_match_returns_original() {
        let rewriter = Rewriter::new(&[Rule {
            r#match: r"^https?://foo\.com/(.*)$".to_string(),
            replace: "https://bar.com/$1".to_string(),
        }])
        .unwrap();

        assert_eq!(
            apply(&rewriter, "https://example.com/x"),
            "https://example.com/x"
        );
    }

    #[test]
    fn applies_first_matching_rule() {
        let rewriter = Rewriter::new(&[
            Rule {
                r#match: r"^https?://www\.reddit\.com/(.*)$".to_string(),
                replace: "https://old.reddit.com/$1".to_string(),
            },
            Rule {
                r#match: r"^https?://reddit\.com/(.*)$".to_string(),
                replace: "https://example.com/$1".to_string(),
            },
        ])
        .unwrap()
        .unwrap();

        assert_eq!(
            rewriter.apply("https://www.reddit.com/r/rust"),
            "https://old.reddit.com/r/rust"
        );
    }

    #[test]
    fn applies_capture_group_rewrites() {
        let rewriter = Rewriter::new(&[Rule {
            r#match: r"^(https://www\.theguardian\.com/uk)$".to_string(),
            replace: "$1/rss".to_string(),
        }])
        .unwrap()
        .unwrap();

        assert_eq!(
            rewriter.apply("https://www.theguardian.com/uk"),
            "https://www.theguardian.com/uk/rss"
        );
    }
}
