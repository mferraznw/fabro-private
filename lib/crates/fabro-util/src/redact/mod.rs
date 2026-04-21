mod entropy;
mod gitleaks;
mod jsonl;

pub use jsonl::{redact_json_value, redact_jsonl_line};
use regex::Regex;
use std::sync::LazyLock;

static AUTH_HEADER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(authorization["']?\s*[:=]\s*["']?\s*bearer\s+)[^"',\s}]+"#).unwrap()
});

static SECRET_ASSIGNMENT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)\b((?:LITELLM_[A-Z0-9_]*API_KEY|[A-Z0-9_]*_LITELLM_KEY|[A-Z0-9_]*API_KEY)["']?\s*[:=]\s*["']?)[^"',\s}]+"#).unwrap()
});

/// A byte range within a string that should be redacted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    pub start: usize,
    pub end: usize,
}

/// Replace all detected secrets in `s` with "REDACTED".
///
/// Uses two layers of detection:
/// 1. Shannon entropy on high-entropy alphanumeric tokens
/// 2. Gitleaks regex pattern matching (200+ known secret formats)
pub fn redact_string(s: &str) -> String {
    let s = redact_known_low_entropy_secrets(s);
    let mut regions = entropy::find_entropy_regions(&s);
    regions.extend(gitleaks::find_gitleaks_regions(&s));

    if regions.is_empty() {
        return s;
    }

    regions.sort_by_key(|r| r.start);

    // Merge overlapping regions
    let mut merged = vec![regions[0].clone()];
    for r in &regions[1..] {
        let last = merged.last_mut().unwrap();
        if r.start <= last.end {
            last.end = last.end.max(r.end);
        } else {
            merged.push(r.clone());
        }
    }

    let mut result = String::with_capacity(s.len());
    let mut prev = 0;
    for r in &merged {
        result.push_str(&s[prev..r.start]);
        result.push_str("REDACTED");
        prev = r.end;
    }
    result.push_str(&s[prev..]);
    result
}

fn redact_known_low_entropy_secrets(s: &str) -> String {
    let value = AUTH_HEADER_RE.replace_all(s, "${1}REDACTED");
    SECRET_ASSIGNMENT_RE
        .replace_all(&value, "${1}REDACTED")
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const HIGH_ENTROPY_SECRET: &str = "sk-ant-api03-xK9mZ2vL8nQ5rT1wY4bC7dF0gH3jE6pA";

    #[test]
    fn redact_string_no_secrets() {
        assert_eq!(redact_string("hello world"), "hello world");
    }

    #[test]
    fn redact_string_with_aws_key() {
        let result = redact_string("key=AKIAYRWQG5EJLPZLBYNP");
        assert_eq!(result, "key=REDACTED");
    }

    #[test]
    fn redact_string_overlapping_detections_produce_single_redacted() {
        // A high-entropy string that also matches a gitleaks pattern
        // should produce one REDACTED, not two
        let input = format!("key={HIGH_ENTROPY_SECRET}");
        let result = redact_string(&input);
        assert_eq!(
            result.matches("REDACTED").count(),
            1,
            "expected single REDACTED, got: {result}"
        );
    }

    #[test]
    fn redact_string_two_secrets_separated_by_space() {
        let input = "key=AKIAYRWQG5EJLPZLBYNP AKIAYRWQG5EJLPZLBYNP";
        let result = redact_string(input);
        assert_eq!(result, "key=REDACTED REDACTED");
    }

    #[test]
    fn redact_string_file_path_preserved() {
        let input = "/tmp/test/controller.go";
        assert_eq!(redact_string(input), input);
    }

    #[test]
    fn redact_string_json_escape_preserved() {
        let input = r"controller.go\nmodel.go";
        assert_eq!(redact_string(input), input);
    }

    #[test]
    fn redact_string_github_pat() {
        let input = "token=ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcdef0123";
        let result = redact_string(input);
        assert!(
            result.contains("REDACTED"),
            "expected REDACTED in: {result}"
        );
    }

    #[test]
    fn redact_string_private_key() {
        let input =
            "-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEA\n-----END RSA PRIVATE KEY-----";
        let result = redact_string(input);
        assert!(
            result.contains("REDACTED"),
            "expected REDACTED in: {result}"
        );
    }

    #[test]
    fn redact_string_litellm_auth_header() {
        let result = redact_string(r"authorization: Bearer litellm-key");
        assert_eq!(result, "authorization: Bearer REDACTED");
    }

    #[test]
    fn redact_string_litellm_api_key_assignment() {
        let result = redact_string("LITELLM_API_KEY=project-key");
        assert_eq!(result, "LITELLM_API_KEY=REDACTED");
    }
}
