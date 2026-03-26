use async_trait::async_trait;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;

/// Provides API tokens, handling refresh when needed.
#[async_trait]
pub trait TokenStore: Send + Sync + std::fmt::Debug {
    /// Get a valid access token, refreshing if expired.
    async fn get_token(&self) -> Result<String, String>;
}

/// Simple static token (API key). No refresh.
#[derive(Debug, Clone)]
pub struct EnvTokenStore {
    key: String,
}

impl EnvTokenStore {
    pub fn new(key: String) -> Self {
        Self { key }
    }
}

#[async_trait]
impl TokenStore for EnvTokenStore {
    async fn get_token(&self) -> Result<String, String> {
        Ok(self.key.clone())
    }
}

/// Configuration for OAuth token refresh.
#[derive(Debug, Clone)]
pub struct OAuthTokenConfig {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: u64, // ms since epoch
    pub issuer: String,
    pub client_id: String,
    pub provider_name: String,
}

/// OAuth token store with automatic refresh.
#[derive(Debug)]
pub struct OAuthTokenStore {
    state: Arc<RwLock<OAuthTokenConfig>>,
    http: reqwest::Client,
    /// Buffer before expiry to trigger refresh (5 minutes).
    buffer_ms: u64,
}

impl OAuthTokenStore {
    pub fn new(config: OAuthTokenConfig) -> Self {
        Self {
            state: Arc::new(RwLock::new(config)),
            http: reqwest::Client::new(),
            buffer_ms: 300_000,
        }
    }

    fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64
    }

    async fn refresh(&self) -> Result<String, String> {
        let state = self.state.read().await;
        let refresh_token = state.refresh_token.clone();
        let issuer = state.issuer.clone();
        let client_id = state.client_id.clone();
        let provider = state.provider_name.clone();
        drop(state);

        tracing::info!(provider = %provider, "Refreshing OAuth token");

        let tokens = fabro_openai_oauth::refresh_access_token(
            &self.http,
            &issuer,
            &client_id,
            &refresh_token,
        )
        .await?;

        let expires_at = Self::now_ms() + tokens.expires_in.unwrap_or(3600) * 1000;

        let mut state = self.state.write().await;
        state.access_token = tokens.access_token.clone();
        state.refresh_token = tokens.refresh_token;
        state.expires_at = expires_at;

        Ok(tokens.access_token)
    }
}

#[async_trait]
impl TokenStore for OAuthTokenStore {
    async fn get_token(&self) -> Result<String, String> {
        let state = self.state.read().await;
        if Self::now_ms() + self.buffer_ms < state.expires_at {
            return Ok(state.access_token.clone());
        }
        drop(state);
        self.refresh().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn env_token_store_returns_static_key() {
        let store = EnvTokenStore::new("test-key-123".to_string());
        let token = store.get_token().await.unwrap();
        assert_eq!(token, "test-key-123");
    }

    #[tokio::test]
    async fn oauth_token_store_returns_cached_when_valid() {
        let store = OAuthTokenStore::new(OAuthTokenConfig {
            access_token: "valid-token".to_string(),
            refresh_token: "refresh-tok".to_string(),
            expires_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64
                + 3_600_000,
            issuer: "https://auth.example.com".to_string(),
            client_id: "test-client".to_string(),
            provider_name: "test".to_string(),
        });
        let token = store.get_token().await.unwrap();
        assert_eq!(token, "valid-token");
    }

    #[tokio::test]
    async fn oauth_token_store_detects_expired() {
        let store = OAuthTokenStore::new(OAuthTokenConfig {
            access_token: "expired-token".to_string(),
            refresh_token: "refresh-tok".to_string(),
            expires_at: 0,
            issuer: "https://auth.example.com".to_string(),
            client_id: "test-client".to_string(),
            provider_name: "test".to_string(),
        });
        // Without a real server, refresh will fail
        let result = store.get_token().await;
        assert!(result.is_err());
    }
}
