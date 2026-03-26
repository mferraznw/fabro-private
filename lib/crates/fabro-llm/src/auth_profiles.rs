//! Loads OAuth credentials from OpenClaw auth profile store.
//!
//! Reads `~/.openclaw/agents/main/agent/auth-profiles.json` to extract
//! Anthropic bearer tokens and Codex OAuth credentials.

use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Deserialize)]
struct AuthProfileStore {
    profiles: std::collections::HashMap<String, serde_json::Value>,
}

/// Codex OAuth credentials extracted from the auth profile store.
#[derive(Debug, Clone)]
pub struct CodexOAuthCreds {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: u64,
    pub account_id: String,
}

fn auth_profiles_path() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".openclaw/agents/main/agent/auth-profiles.json")
}

fn load_store() -> Option<AuthProfileStore> {
    let path = auth_profiles_path();
    let data = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&data).ok()
}

/// Load the Anthropic bearer token from OpenClaw auth profiles.
/// Returns the token string if found (sk-ant-oat01-...).
pub fn load_anthropic_token() -> Option<String> {
    let store = load_store()?;
    let profile = store.profiles.get("anthropic:oauth")?;
    if profile.get("type")?.as_str()? != "token" {
        return None;
    }
    profile.get("token")?.as_str().map(String::from)
}

/// Load Codex OAuth credentials from OpenClaw auth profiles.
pub fn load_codex_oauth() -> Option<CodexOAuthCreds> {
    let store = load_store()?;
    let profile = store.profiles.get("openai-codex:default")?;
    if profile.get("type")?.as_str()? != "oauth" {
        return None;
    }
    Some(CodexOAuthCreds {
        access_token: profile.get("access")?.as_str()?.to_string(),
        refresh_token: profile.get("refresh")?.as_str()?.to_string(),
        expires_at: profile.get("expires")?.as_u64()?,
        account_id: profile.get("accountId")?.as_str()?.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_anthropic_token_from_json() {
        let json = r#"{
            "profiles": {
                "anthropic:oauth": { "type": "token", "provider": "anthropic", "token": "sk-ant-test-123" }
            }
        }"#;
        let store: AuthProfileStore = serde_json::from_str(json).unwrap();
        let profile = store.profiles.get("anthropic:oauth").unwrap();
        assert_eq!(
            profile.get("token").unwrap().as_str().unwrap(),
            "sk-ant-test-123"
        );
    }

    #[test]
    fn parse_codex_oauth_from_json() {
        let json = r#"{
            "profiles": {
                "openai-codex:default": {
                    "type": "oauth",
                    "provider": "openai-codex",
                    "access": "eyJ...",
                    "refresh": "rt_test",
                    "expires": 9999999999999,
                    "accountId": "acct-123"
                }
            }
        }"#;
        let store: AuthProfileStore = serde_json::from_str(json).unwrap();
        let profile = store.profiles.get("openai-codex:default").unwrap();
        assert_eq!(profile.get("type").unwrap().as_str().unwrap(), "oauth");
        assert_eq!(
            profile.get("accountId").unwrap().as_str().unwrap(),
            "acct-123"
        );
    }
}
