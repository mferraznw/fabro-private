# Provider OAuth, ADO Integration & Web Auth Enhancement Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Enable OAuth token refresh for Anthropic and OpenAI Codex providers, add LiteLLM proxy support (spark-glm-4.7-flash), add Azure Foundry model support, integrate Azure DevOps for git operations, and enhance web auth with Google OAuth and disable-auth-for-testing support.

**Architecture:** Six independent workstreams that can be developed in parallel. Each touches a different subsystem: (1) LLM token management via a new `TokenStore` abstraction in `fabro-llm`, (2) a new `spark` provider via the existing `OpenAiCompatibleAdapter`, (3) Azure Foundry as two new OpenAI-compatible endpoints, (4) a `fabro-ado` crate mirroring `fabro-github` with PAT-based REST, (5) web auth provider abstraction in the React app, (6) ADO pipeline integration as workflow shell nodes.

**Tech Stack:** Rust (tokio, reqwest, serde, axum), TypeScript (React Router, Arctic OAuth), Azure DevOps REST API v7, LiteLLM proxy, Google OAuth 2.0

---

## Workstream 1: OAuth Token Refresh for Anthropic & Codex

### Context

- OpenClaw stores OAuth creds in `~/.openclaw/agents/main/agent/auth-profiles.json`
- Anthropic OAuth token: `sk-ant-oat01-...` (stored as `type: "token"` with no refresh — static bearer)
- Codex OAuth: `type: "oauth"` with `access`, `refresh`, `expires`, `accountId` fields
- Fabro already has `fabro-openai-oauth` crate with `refresh_access_token()` — needs wiring into the LLM client
- Gemini has OAuth creds at `~/.gemini/oauth_creds.json` with `refresh_token`

### Task 1.1: Add `TokenStore` trait to `fabro-llm`

**Files:**
- Create: `lib/crates/fabro-llm/src/token_store.rs`
- Modify: `lib/crates/fabro-llm/src/lib.rs` (add module)

**Step 1: Write the failing test**

```rust
// In token_store.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn env_token_store_returns_static_key() {
        let store = EnvTokenStore::new("test-key-123".to_string());
        let token = store.get_token().await.unwrap();
        assert_eq!(token, "test-key-123");
    }
}
```

**Step 2: Run test to verify it fails**

Run: `cargo nextest run -p fabro-llm -- env_token_store`
Expected: FAIL — module doesn't exist

**Step 3: Write minimal implementation**

```rust
use async_trait::async_trait;

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
```

**Step 4: Run test to verify it passes**

Run: `cargo nextest run -p fabro-llm -- env_token_store`
Expected: PASS

**Step 5: Commit**

```bash
git add lib/crates/fabro-llm/src/token_store.rs lib/crates/fabro-llm/src/lib.rs
git commit -m "feat(fabro-llm): add TokenStore trait for pluggable token management"
```

---

### Task 1.2: Add `OAuthTokenStore` with refresh logic

**Files:**
- Modify: `lib/crates/fabro-llm/src/token_store.rs`
- Modify: `lib/crates/fabro-llm/Cargo.toml` (add `tokio` sync dependency if not present)

**Step 1: Write the failing test**

```rust
#[tokio::test]
async fn oauth_token_store_returns_cached_when_valid() {
    let store = OAuthTokenStore::new(OAuthTokenConfig {
        access_token: "valid-token".to_string(),
        refresh_token: "refresh-tok".to_string(),
        expires_at: SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64 + 3_600_000,
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
        expires_at: 0, // epoch = expired
        issuer: "https://auth.example.com".to_string(),
        client_id: "test-client".to_string(),
        provider_name: "test".to_string(),
    });
    // Without a real server, refresh will fail
    let result = store.get_token().await;
    assert!(result.is_err());
}
```

**Step 2: Run test to verify it fails**

Run: `cargo nextest run -p fabro-llm -- oauth_token_store`
Expected: FAIL

**Step 3: Write implementation**

```rust
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;

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
            &self.http, &issuer, &client_id, &refresh_token,
        ).await?;

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
```

**Step 4: Run test to verify it passes**

Run: `cargo nextest run -p fabro-llm -- oauth_token_store`
Expected: PASS

**Step 5: Commit**

```bash
git add lib/crates/fabro-llm/src/token_store.rs lib/crates/fabro-llm/Cargo.toml
git commit -m "feat(fabro-llm): add OAuthTokenStore with automatic token refresh"
```

---

### Task 1.3: Wire `TokenStore` into `ProviderAdapter` implementations

**Files:**
- Modify: `lib/crates/fabro-llm/src/providers/anthropic.rs` — accept optional `TokenStore`
- Modify: `lib/crates/fabro-llm/src/providers/openai.rs` — accept optional `TokenStore`
- Modify: `lib/crates/fabro-llm/src/client.rs` — use token stores during `from_env()`

**Step 1: Modify `AnthropicAdapter` to accept `TokenStore`**

Add a `token_store: Option<Arc<dyn TokenStore>>` field. In `complete()` and `stream()`, if token_store is set, call `get_token()` instead of using the static `api_key` field for the `x-api-key` header.

**Step 2: Modify `OpenAiAdapter` similarly**

Same pattern — `token_store` field, `get_token()` on each request.

**Step 3: Wire in `from_env()` — detect OAuth credentials**

In `Client::from_env()`, check for:
- `ANTHROPIC_OAUTH_TOKEN` + `ANTHROPIC_REFRESH_TOKEN` → create `OAuthTokenStore` for Anthropic
- `OPENAI_REFRESH_TOKEN` + `CHATGPT_ACCOUNT_ID` → create `OAuthTokenStore` for Codex
- Fall back to existing `ANTHROPIC_API_KEY` / `OPENAI_API_KEY` static key paths

Also check `~/.openclaw/agents/main/agent/auth-profiles.json` as a token source:
- Read `anthropic:oauth` profile → use `token` field as static bearer
- Read `openai-codex:default` profile → use `access`/`refresh`/`expires`/`accountId`

**Step 4: Run existing tests**

Run: `cargo nextest run -p fabro-llm`
Expected: All existing tests still pass

**Step 5: Commit**

```bash
git add lib/crates/fabro-llm/src/providers/anthropic.rs lib/crates/fabro-llm/src/providers/openai.rs lib/crates/fabro-llm/src/client.rs
git commit -m "feat(fabro-llm): wire TokenStore into Anthropic and OpenAI providers for OAuth refresh"
```

---

### Task 1.4: Add `AuthProfileStore` loader for OpenClaw credentials

**Files:**
- Create: `lib/crates/fabro-llm/src/auth_profiles.rs`
- Modify: `lib/crates/fabro-llm/src/lib.rs`

**Step 1: Write the test**

```rust
#[test]
fn parse_openclaw_auth_profiles() {
    let json = r#"{
        "version": 1,
        "profiles": {
            "anthropic:oauth": { "type": "token", "provider": "anthropic", "token": "sk-ant-test" },
            "openai-codex:default": { "type": "oauth", "provider": "openai-codex",
                "access": "eyJ...", "refresh": "rt_test", "expires": 9999999999999, "accountId": "acct-123" }
        }
    }"#;
    let store: AuthProfileStore = serde_json::from_str(json).unwrap();
    assert_eq!(store.profiles.len(), 2);
    let anthropic = store.get_anthropic_token();
    assert_eq!(anthropic, Some("sk-ant-test".to_string()));
    let codex = store.get_codex_oauth();
    assert!(codex.is_some());
    let codex = codex.unwrap();
    assert_eq!(codex.account_id, "acct-123");
}
```

**Step 2: Implement the loader**

Deserialize `~/.openclaw/agents/main/agent/auth-profiles.json`, extract Anthropic token and Codex OAuth credentials. Return structured config that `Client::from_env()` can consume.

**Step 3: Run tests**

Run: `cargo nextest run -p fabro-llm -- parse_openclaw`
Expected: PASS

**Step 4: Commit**

```bash
git add lib/crates/fabro-llm/src/auth_profiles.rs lib/crates/fabro-llm/src/lib.rs
git commit -m "feat(fabro-llm): add AuthProfileStore loader for OpenClaw credentials"
```

---

## Workstream 2: LiteLLM Proxy — spark-glm-4.7-flash

### Context

- LiteLLM config at `~/.litellm/config.yaml` defines `glm-4.7-flash` model
- Two endpoints: `http://100.124.192.31:8000/v1` and `http://100.96.250.110:8000/v1` (Tailscale IPs)
- API key: `none` (no auth needed)
- Max tokens: 65535, function calling supported
- LiteLLM proxy runs locally and load-balances across endpoints
- This is a standard OpenAI-compatible endpoint

### Task 2.1: Add `spark` provider to catalog and client

**Files:**
- Modify: `lib/crates/fabro-model/src/catalog.json` — add `spark-glm-4.7-flash` model
- Modify: `lib/crates/fabro-model/src/provider.rs` — add `Spark` variant (or use `OpenAiCompatible`)
- Modify: `lib/crates/fabro-llm/src/client.rs` — register from env
- Modify: `.env.example` — add `SPARK_API_BASE`

**Step 1: Add model to catalog**

Add to `catalog.json`:
```json
{
  "id": "spark-glm-4.7-flash",
  "provider": "openai_compatible",
  "name": "GLM 4.7 Flash (Spark LiteLLM)",
  "context_window": 65535,
  "max_output_tokens": 65535,
  "features": { "tools": true, "vision": false, "reasoning": false },
  "input_cost_per_mtok": 0,
  "output_cost_per_mtok": 0,
  "aliases": ["spark", "glm-flash"]
}
```

**Step 2: Register in `from_env()`**

Check for `SPARK_API_BASE` env var (default: `http://100.124.192.31:8000/v1`):
```rust
if let Ok(base) = std::env::var("SPARK_API_BASE") {
    let adapter = providers::OpenAiCompatibleAdapter::new(
        std::env::var("SPARK_API_KEY").unwrap_or_else(|_| "none".to_string()),
        &base,
    ).with_name("spark");
    client.register_provider(Arc::new(adapter)).await?;
}
```

**Step 3: Run existing tests**

Run: `cargo nextest run -p fabro-llm -p fabro-model`
Expected: PASS

**Step 4: Commit**

```bash
git add lib/crates/fabro-model/src/catalog.json lib/crates/fabro-llm/src/client.rs .env.example
git commit -m "feat: add spark-glm-4.7-flash provider via LiteLLM proxy"
```

---

## Workstream 3: Azure Foundry Models

### Context

Azure AI Foundry exposes models via two API formats:
- **OpenAI-compatible endpoint**: `https://<resource>.openai.azure.com/openai/deployments/<deployment>/chat/completions?api-version=2024-10-21`
- **Anthropic-compatible endpoint**: `https://<resource>.services.ai.azure.com/anthropic/v1/messages` (for Claude models hosted on Azure)

Both use `api-key` header authentication.

### Task 3.1: Add Azure OpenAI provider

**Files:**
- Modify: `lib/crates/fabro-llm/src/client.rs`
- Modify: `lib/crates/fabro-model/src/catalog.json`
- Modify: `.env.example`

**Step 1: Register Azure OpenAI in `from_env()`**

```rust
if let Ok(base_url) = std::env::var("AZURE_OPENAI_BASE_URL") {
    let key = std::env::var("AZURE_OPENAI_API_KEY")
        .map_err(|_| SdkError::Configuration("AZURE_OPENAI_API_KEY required with AZURE_OPENAI_BASE_URL".into()))?;
    let adapter = providers::OpenAiCompatibleAdapter::new(key, &base_url)
        .with_name("azure-openai")
        .with_auth_header("api-key"); // Azure uses api-key header, not Authorization Bearer
    client.register_provider(Arc::new(adapter)).await?;
}
```

**Step 2: Add `with_auth_header()` to `OpenAiCompatibleAdapter`**

Azure uses `api-key: <key>` instead of `Authorization: Bearer <key>`. Add a configurable auth header name to the adapter.

**Step 3: Run tests**

Run: `cargo nextest run -p fabro-llm`
Expected: PASS

**Step 4: Commit**

```bash
git add lib/crates/fabro-llm/src/client.rs lib/crates/fabro-llm/src/providers/openai_compatible.rs lib/crates/fabro-model/src/catalog.json .env.example
git commit -m "feat: add Azure OpenAI Foundry provider support"
```

---

### Task 3.2: Add Azure Anthropic provider

**Files:**
- Modify: `lib/crates/fabro-llm/src/providers/anthropic.rs`
- Modify: `lib/crates/fabro-llm/src/client.rs`

**Step 1: Register Azure Anthropic in `from_env()`**

```rust
if let Ok(base_url) = std::env::var("AZURE_ANTHROPIC_BASE_URL") {
    let key = std::env::var("AZURE_ANTHROPIC_API_KEY")
        .map_err(|_| SdkError::Configuration("AZURE_ANTHROPIC_API_KEY required".into()))?;
    let mut adapter = providers::AnthropicAdapter::new(key)
        .with_base_url(base_url);
    // Azure Anthropic uses api-key header instead of x-api-key
    adapter = adapter.with_auth_header("api-key");
    client.register_provider_as(Arc::new(adapter), "azure-anthropic").await?;
}
```

**Step 2: Add `with_auth_header()` to `AnthropicAdapter`**

Similar to the OpenAI compatible change — configurable auth header name.

**Step 3: Run tests, commit**

```bash
git commit -m "feat: add Azure Anthropic Foundry provider support"
```

---

## Workstream 4: Azure DevOps Git Integration

### Context

- ADO org: `https://dev.azure.com/netwoveninc`
- Default project: `O365Governance`
- PAT: available in `~/.claude/mcp_config.json` and `~/.azure/azuredevops/config`
- ADO REST API v7: `https://dev.azure.com/{org}/{project}/_apis/git/...`
- Already have `@tiberriver256/mcp-server-azure-devops` in Claude MCP config
- The `fabro-github` crate is tightly coupled — no trait abstraction

### Task 4.1: Create `fabro-ado` crate — core REST client

**Files:**
- Create: `lib/crates/fabro-ado/Cargo.toml`
- Create: `lib/crates/fabro-ado/src/lib.rs`
- Modify: `Cargo.toml` (workspace members)

**Step 1: Write the test**

```rust
#[test]
fn parse_ado_repo_url() {
    let (org, project, repo) = parse_ado_url(
        "https://dev.azure.com/netwoveninc/O365Governance/_git/MyRepo"
    ).unwrap();
    assert_eq!(org, "netwoveninc");
    assert_eq!(project, "O365Governance");
    assert_eq!(repo, "MyRepo");
}
```

**Step 2: Implement core client**

```rust
pub struct AdoClient {
    http: reqwest::Client,
    org_url: String,
    pat: String,
}

impl AdoClient {
    pub fn new(org_url: &str, pat: &str) -> Self { ... }
    pub async fn create_pull_request(&self, project: &str, repo: &str, pr: &CreatePrRequest) -> Result<PullRequest> { ... }
    pub async fn list_pull_requests(&self, project: &str, repo: &str) -> Result<Vec<PullRequest>> { ... }
    pub async fn merge_pull_request(&self, ...) -> Result<()> { ... }
    pub async fn create_work_item(&self, project: &str, work_item_type: &str, ...) -> Result<WorkItem> { ... }
}
```

PAT auth: `Authorization: Basic base64(":{pat}")`

**Step 3: Run tests, commit**

```bash
git commit -m "feat: add fabro-ado crate with ADO REST client for PRs and work items"
```

---

### Task 4.2: Add `GitProvider` trait and ADO variant

**Files:**
- Modify: `lib/crates/fabro-config/src/server.rs` — add `AzureDevops` variant to `GitProvider`
- Create: `lib/crates/fabro-workflows/src/git_provider.rs` — trait abstracting PR operations

**Step 1: Extend enum**

```rust
pub enum GitProvider {
    #[default]
    Github,
    AzureDevops,
}
```

**Step 2: Add config fields**

```rust
pub struct GitConfig {
    pub provider: GitProvider,
    // GitHub fields
    pub app_id: Option<String>,
    pub client_id: Option<String>,
    pub slug: Option<String>,
    // ADO fields
    pub ado_org_url: Option<String>,
    pub ado_pat: Option<String>,
    pub ado_default_project: Option<String>,
    // Common
    pub author: GitAuthorConfig,
    pub webhooks: Option<WebhookConfig>,
}
```

**Step 3: Run tests, commit**

```bash
git commit -m "feat: add AzureDevops variant to GitProvider and ADO config fields"
```

---

### Task 4.3: Wire ADO into workflow PR creation

**Files:**
- Modify: `lib/crates/fabro-workflows/src/pull_request.rs` — branch on `GitProvider`
- Modify: relevant CLI commands if needed

**Step 1: In `maybe_open_pull_request()`, check provider**

If `GitProvider::AzureDevops`, use `fabro_ado::AdoClient` instead of `fabro_github`. Parse repo URL as ADO format, create PR via ADO REST API.

**Step 2: Run tests, commit**

```bash
git commit -m "feat: wire ADO pull request creation into workflow engine"
```

---

### Task 4.4: ADO pipeline integration as workflow nodes

**Context from user:** ADO pipeline triggers, timeline data, work item creation can all be done via shell nodes in the workflow graph — no core modification needed.

**Files:**
- Create: `fabro/workflows/ado-pipeline-trigger/workflow.toml` (example workflow)

**Step 1: Create example workflow**

A workflow graph that:
1. Triggers an ADO pipeline run via `az pipelines run --name <pipeline> --branch <branch>`
2. Polls for completion via `az pipelines runs show --id <run-id>`
3. Extracts timeline/duration data as eval input
4. Creates work items for failures via `az boards work-item create`

This uses existing shell/command node types — no engine changes.

**Step 2: Commit**

```bash
git commit -m "feat: add example ADO pipeline trigger workflow"
```

---

## Workstream 5: Web Auth Enhancement

### Context

- Current: GitHub OAuth only, or `insecure_disabled` (demo)
- Need: Google OAuth, disable-auth-for-testing toggle
- Session stores GitHub-specific fields
- Uses Arctic OAuth library (supports Google)
- `server.toml` config drives auth provider selection

### Task 5.1: Add Google OAuth provider to web app

**Files:**
- Modify: `apps/fabro-web/app/lib/config.server.ts` — add `"google"` provider
- Create: `apps/fabro-web/app/lib/google.server.ts` — Google OAuth setup
- Modify: `apps/fabro-web/app/routes/auth-login.tsx` — branch on provider
- Modify: `apps/fabro-web/app/routes/auth-callback.tsx` — handle Google callback
- Modify: `apps/fabro-web/app/lib/session.server.ts` — generalize session fields

**Step 1: Add Google OAuth config**

In `config.server.ts`, add `"google"` to the `AuthConfig.provider` union type.

**Step 2: Create `google.server.ts`**

```typescript
import { Google } from "arctic";
import { getAppConfig } from "./config.server";

export function getGoogleOAuth(): Google | null {
    const config = getAppConfig();
    const clientId = config.web.auth.google_client_id;
    const clientSecret = process.env.GOOGLE_CLIENT_SECRET;
    if (!clientId || !clientSecret) return null;
    const redirectUri = `${config.web.url}/auth/callback`;
    return new Google(clientId, clientSecret, redirectUri);
}
```

**Step 3: Update login route**

In `auth-login.tsx`, check `config.web.auth.provider`:
- `"github"` → existing GitHub flow
- `"google"` → Google OAuth with scopes `["openid", "email", "profile"]`

**Step 4: Update callback route**

In `auth-callback.tsx`, detect provider from state/session, exchange code, fetch user profile from Google userinfo endpoint.

**Step 5: Generalize session**

Rename `githubId` → `userId`, `githubNodeId` → `providerNodeId`. Keep backwards compatible.

**Step 6: Run tests, commit**

```bash
git commit -m "feat(web): add Google OAuth login support"
```

---

### Task 5.2: Update `server.toml` and `AuthProvider` enum (Rust side)

**Files:**
- Modify: `lib/crates/fabro-config/src/server.rs` — add `Google` variant

```rust
pub enum AuthProvider {
    #[default]
    Github,
    Google,
    InsecureDisabled,
}
```

**Commit:**

```bash
git commit -m "feat(config): add Google auth provider variant"
```

---

### Task 5.3: Add `insecure_disabled` config for testing

**Files:**
- Modify: `apps/fabro-web/app/layouts/app-shell.tsx`

The existing `insecure_disabled` path already works (line 44). The issue was config loading. Ensure `server.toml` with `auth.provider = "insecure_disabled"` works without `FABRO_DEMO=1` env var.

Verify the TOML parsing handles `[web.auth]` nested tables correctly with `smol-toml`. If needed, add a test.

**Commit:**

```bash
git commit -m "fix(web): ensure insecure_disabled auth works via server.toml without FABRO_DEMO"
```

---

## Workstream 6: Environment Setup & Configuration

### Task 6.1: Update `.env.example` with all new variables

**Files:**
- Modify: `.env.example`

Add:
```bash
# OAuth (alternative to API keys)
ANTHROPIC_OAUTH_TOKEN=
ANTHROPIC_REFRESH_TOKEN=
OPENAI_REFRESH_TOKEN=
CHATGPT_ACCOUNT_ID=

# Spark LiteLLM proxy
SPARK_API_BASE=http://100.124.192.31:8000/v1
SPARK_API_KEY=none

# Azure Foundry
AZURE_OPENAI_BASE_URL=
AZURE_OPENAI_API_KEY=
AZURE_ANTHROPIC_BASE_URL=
AZURE_ANTHROPIC_API_KEY=

# Azure DevOps
ADO_ORG_URL=https://dev.azure.com/netwoveninc
ADO_PAT=
ADO_DEFAULT_PROJECT=O365Governance

# Google OAuth (web)
GOOGLE_CLIENT_ID=
GOOGLE_CLIENT_SECRET=
```

**Commit:**

```bash
git commit -m "docs: update .env.example with OAuth, Azure Foundry, ADO, and Google auth vars"
```

---

### Task 6.2: Update `server.toml` template

**Files:**
- Modify: docs or create `files-internal/server.toml.example`

Example config for the user's setup:
```toml
[api]
base_url = "http://localhost:3111"
authentication_strategy = "insecure_disabled"

[web]
url = "http://localhost:3112"

[web.auth]
provider = "google"
google_client_id = "..."

[git]
provider = "azuredevops"
ado_org_url = "https://dev.azure.com/netwoveninc"
ado_default_project = "O365Governance"
```

**Commit:**

```bash
git commit -m "docs: add server.toml example with ADO and Google auth config"
```

---

## Workstream 7: Repo Git Config for ADO

### Context

The repo currently uses GitHub remote. For ADO operations, the repo's `.git/config` needs an ADO remote. This may be as simple as:

```bash
git remote add ado https://dev.azure.com/netwoveninc/O365Governance/_git/fabro
```

Or the user may want to keep GitHub as origin and ADO as a secondary. The `fabro-ado` crate should detect the remote URL format to determine which git provider to use.

### Task 7.1: Remote URL detection

**Files:**
- Modify: `lib/crates/fabro-ado/src/lib.rs`

**Step 1: Write test**

```rust
#[test]
fn detect_ado_remote() {
    assert!(is_ado_url("https://dev.azure.com/org/project/_git/repo"));
    assert!(is_ado_url("https://org@dev.azure.com/org/project/_git/repo"));
    assert!(!is_ado_url("https://github.com/org/repo.git"));
}
```

**Step 2: Implement**

Parse git remote URL to detect ADO vs GitHub. Use this in workflow PR creation to auto-select provider.

**Step 3: Commit**

```bash
git commit -m "feat(fabro-ado): add remote URL detection for auto provider selection"
```

---

## Execution Order (Recommended)

Independent workstreams can be parallelized:

```
Workstream 1 (OAuth)     ████████████████████  (largest — 4 tasks)
Workstream 2 (Spark)     ████                  (1 task, quick)
Workstream 3 (Azure)     ████████              (2 tasks)
Workstream 4 (ADO Git)   ████████████████      (4 tasks)
Workstream 5 (Web Auth)  ████████████          (3 tasks)
Workstream 6 (Config)    ████                  (2 tasks, do last)
Workstream 7 (Git Remote)████                  (1 task)
```

**Start with:** Workstreams 2 (Spark) and 6 (Config) — quickest wins.
**Then:** Workstreams 1 (OAuth) and 3 (Azure) in parallel — LLM provider work.
**Then:** Workstream 4 (ADO) and 5 (Web Auth) — infrastructure work.
**Last:** Workstream 7 (Git Remote) — depends on 4.

---

## Testing Strategy

- **Unit tests:** Each task includes tests. Run with `cargo nextest run -p <crate>`
- **E2E (OAuth):** Requires real tokens. Use `--run-ignored only` profile with `.env` credentials
- **E2E (ADO):** Requires PAT. Tag with `#[ignore]` for CI, run locally with `ADO_PAT` set
- **Web:** `cd apps/fabro-web && bun test` for session/auth tests
- **Integration:** Start API server + web app, verify login flow manually

## Notes

- The Anthropic "OAuth" token from OpenClaw (`sk-ant-oat01-...`) is a static bearer token, not a refreshable OAuth token. It's treated as `type: "token"` in OpenClaw. For Fabro, this is equivalent to an API key — just use it as `ANTHROPIC_API_KEY`.
- The Codex OAuth is the real refresh flow — tokens expire and need `refresh_access_token()`.
- ADO pipeline integration doesn't require Fabro core changes — shell nodes in workflow graphs handle it.
- The ADO MCP server (`@tiberriver256/mcp-server-azure-devops`) is already configured in Claude — can be used for ad-hoc ADO operations without building into Fabro itself.
