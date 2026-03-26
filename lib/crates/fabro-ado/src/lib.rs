//! Azure DevOps REST API client.
//!
//! Supports pull request operations and work item creation via PAT authentication.

use anyhow::{Context, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};

/// Azure DevOps REST API client.
#[derive(Clone)]
pub struct AdoClient {
    http: reqwest::Client,
    org_url: String,
    /// Base64-encoded PAT for Basic auth.
    auth_header: String,
}

impl AdoClient {
    /// Create a new client. `org_url` should be like `https://dev.azure.com/myorg`.
    pub fn new(org_url: &str, pat: &str) -> Self {
        let encoded = base64::engine::general_purpose::STANDARD.encode(format!(":{pat}"));
        Self {
            http: reqwest::Client::new(),
            org_url: org_url.trim_end_matches('/').to_string(),
            auth_header: format!("Basic {encoded}"),
        }
    }

    /// Create from environment variables `ADO_ORG_URL` and `ADO_PAT`.
    pub fn from_env() -> Option<Self> {
        let org_url = std::env::var("ADO_ORG_URL").ok()?;
        let pat = std::env::var("ADO_PAT").ok()?;
        Some(Self::new(&org_url, &pat))
    }

    fn api_url(&self, project: &str, path: &str) -> String {
        format!(
            "{}/{}/_apis/{}?api-version=7.1",
            self.org_url, project, path
        )
    }

    /// Create a pull request.
    pub async fn create_pull_request(
        &self,
        project: &str,
        repo: &str,
        pr: &CreatePrRequest,
    ) -> Result<PullRequestResponse> {
        let url = self.api_url(project, &format!("git/repositories/{repo}/pullrequests"));
        let resp = self
            .http
            .post(&url)
            .header("Authorization", &self.auth_header)
            .header("Content-Type", "application/json")
            .json(pr)
            .send()
            .await
            .context("Failed to create pull request")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("ADO API error ({status}): {body}");
        }

        resp.json().await.context("Failed to parse PR response")
    }

    /// List active pull requests for a repository.
    pub async fn list_pull_requests(
        &self,
        project: &str,
        repo: &str,
    ) -> Result<Vec<PullRequestResponse>> {
        let url = self.api_url(project, &format!("git/repositories/{repo}/pullrequests"));
        let resp = self
            .http
            .get(&url)
            .header("Authorization", &self.auth_header)
            .send()
            .await
            .context("Failed to list pull requests")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("ADO API error ({status}): {body}");
        }

        let list: PullRequestListResponse = resp.json().await.context("Failed to parse PR list")?;
        Ok(list.value)
    }

    /// Complete (merge) a pull request.
    pub async fn complete_pull_request(
        &self,
        project: &str,
        repo: &str,
        pr_id: u64,
        merge_strategy: MergeStrategy,
        delete_source_branch: bool,
    ) -> Result<PullRequestResponse> {
        let url = self.api_url(
            project,
            &format!("git/repositories/{repo}/pullrequests/{pr_id}"),
        );

        let last_merge_source_commit = {
            let pr = self.get_pull_request(project, repo, pr_id).await?;
            pr.last_merge_source_commit
                .map(|c| c.commit_id)
                .unwrap_or_default()
        };

        let body = serde_json::json!({
            "status": "completed",
            "completionOptions": {
                "mergeStrategy": merge_strategy.as_str(),
                "deleteSourceBranch": delete_source_branch,
                "mergeCommitMessage": format!("Merged PR {pr_id}"),
            },
            "lastMergeSourceCommit": {
                "commitId": last_merge_source_commit,
            }
        });

        let resp = self
            .http
            .patch(&url)
            .header("Authorization", &self.auth_header)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .context("Failed to complete pull request")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("ADO API error ({status}): {body}");
        }

        resp.json().await.context("Failed to parse PR response")
    }

    /// Get a single pull request by ID.
    pub async fn get_pull_request(
        &self,
        project: &str,
        repo: &str,
        pr_id: u64,
    ) -> Result<PullRequestResponse> {
        let url = self.api_url(
            project,
            &format!("git/repositories/{repo}/pullrequests/{pr_id}"),
        );
        let resp = self
            .http
            .get(&url)
            .header("Authorization", &self.auth_header)
            .send()
            .await
            .context("Failed to get pull request")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("ADO API error ({status}): {body}");
        }

        resp.json().await.context("Failed to parse PR response")
    }

    /// Create a work item (Bug, Task, User Story, etc.).
    pub async fn create_work_item(
        &self,
        project: &str,
        work_item_type: &str,
        title: &str,
        description: Option<&str>,
    ) -> Result<WorkItemResponse> {
        let url = format!(
            "{}/{}/_apis/wit/workitems/${}?api-version=7.1",
            self.org_url, project, work_item_type
        );

        let mut ops = vec![serde_json::json!({
            "op": "add",
            "path": "/fields/System.Title",
            "value": title
        })];

        if let Some(desc) = description {
            ops.push(serde_json::json!({
                "op": "add",
                "path": "/fields/System.Description",
                "value": desc
            }));
        }

        let resp = self
            .http
            .post(&url)
            .header("Authorization", &self.auth_header)
            .header("Content-Type", "application/json-patch+json")
            .json(&ops)
            .send()
            .await
            .context("Failed to create work item")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("ADO API error ({status}): {body}");
        }

        resp.json()
            .await
            .context("Failed to parse work item response")
    }

    /// Trigger a pipeline run.
    pub async fn run_pipeline(
        &self,
        project: &str,
        pipeline_id: u64,
        branch: &str,
    ) -> Result<PipelineRunResponse> {
        let url = self.api_url(project, &format!("pipelines/{pipeline_id}/runs"));
        let body = serde_json::json!({
            "resources": {
                "repositories": {
                    "self": {
                        "refName": format!("refs/heads/{branch}")
                    }
                }
            }
        });

        let resp = self
            .http
            .post(&url)
            .header("Authorization", &self.auth_header)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .context("Failed to trigger pipeline")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("ADO API error ({status}): {body}");
        }

        resp.json()
            .await
            .context("Failed to parse pipeline response")
    }

    /// Get pipeline run status.
    pub async fn get_pipeline_run(
        &self,
        project: &str,
        pipeline_id: u64,
        run_id: u64,
    ) -> Result<PipelineRunResponse> {
        let url = self.api_url(project, &format!("pipelines/{pipeline_id}/runs/{run_id}"));
        let resp = self
            .http
            .get(&url)
            .header("Authorization", &self.auth_header)
            .send()
            .await
            .context("Failed to get pipeline run")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("ADO API error ({status}): {body}");
        }

        resp.json()
            .await
            .context("Failed to parse pipeline run response")
    }
}

// ---------------------------------------------------------------------------
// Request/response types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatePrRequest {
    pub source_ref_name: String,
    pub target_ref_name: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PullRequestListResponse {
    value: Vec<PullRequestResponse>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestResponse {
    pub pull_request_id: u64,
    pub title: String,
    pub status: String,
    #[serde(default)]
    pub source_ref_name: String,
    #[serde(default)]
    pub target_ref_name: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub last_merge_source_commit: Option<CommitRef>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitRef {
    pub commit_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkItemResponse {
    pub id: u64,
    pub url: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelineRunResponse {
    pub id: u64,
    pub state: String,
    #[serde(default)]
    pub result: Option<String>,
    pub url: String,
}

/// Merge strategy for completing pull requests.
#[derive(Debug, Clone, Copy)]
pub enum MergeStrategy {
    NoFastForward,
    Squash,
    Rebase,
    RebaseMerge,
}

impl MergeStrategy {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NoFastForward => "noFastForward",
            Self::Squash => "squash",
            Self::Rebase => "rebase",
            Self::RebaseMerge => "rebaseMerge",
        }
    }
}

// ---------------------------------------------------------------------------
// URL parsing
// ---------------------------------------------------------------------------

/// Parse an Azure DevOps repository URL into (org, project, repo).
///
/// Supports:
/// - `https://dev.azure.com/{org}/{project}/_git/{repo}`
/// - `https://{org}@dev.azure.com/{org}/{project}/_git/{repo}`
pub fn parse_ado_url(url: &str) -> Option<(String, String, String)> {
    let url = url.trim_end_matches('/').trim_end_matches(".git");

    // Find _git/ segment
    let git_idx = url.find("/_git/")?;
    let repo = url[git_idx + 6..].to_string();

    // Everything before /_git/ contains org and project
    let prefix = &url[..git_idx];

    // Try https://dev.azure.com/{org}/{project}
    if let Some(rest) = prefix.strip_prefix("https://dev.azure.com/").or_else(|| {
        // https://{org}@dev.azure.com/{org}/{project}
        let at_idx = prefix.find("@dev.azure.com/")?;
        Some(&prefix[at_idx + "@dev.azure.com/".len()..])
    }) {
        let parts: Vec<&str> = rest.splitn(2, '/').collect();
        if parts.len() == 2 {
            return Some((parts[0].to_string(), parts[1].to_string(), repo));
        }
    }

    None
}

/// Check if a URL is an Azure DevOps URL.
pub fn is_ado_url(url: &str) -> bool {
    url.contains("dev.azure.com") && url.contains("/_git/")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_standard_ado_url() {
        let (org, project, repo) =
            parse_ado_url("https://dev.azure.com/netwoveninc/O365Governance/_git/MyRepo").unwrap();
        assert_eq!(org, "netwoveninc");
        assert_eq!(project, "O365Governance");
        assert_eq!(repo, "MyRepo");
    }

    #[test]
    fn parse_ado_url_with_user_prefix() {
        let (org, project, repo) =
            parse_ado_url("https://user@dev.azure.com/netwoveninc/O365Governance/_git/MyRepo")
                .unwrap();
        assert_eq!(org, "netwoveninc");
        assert_eq!(project, "O365Governance");
        assert_eq!(repo, "MyRepo");
    }

    #[test]
    fn parse_ado_url_with_trailing_git() {
        let result = parse_ado_url("https://dev.azure.com/org/proj/_git/repo.git");
        assert!(result.is_some());
        let (_, _, repo) = result.unwrap();
        assert_eq!(repo, "repo");
    }

    #[test]
    fn parse_non_ado_url_returns_none() {
        assert!(parse_ado_url("https://github.com/org/repo.git").is_none());
    }

    #[test]
    fn is_ado_url_detection() {
        assert!(is_ado_url("https://dev.azure.com/org/proj/_git/repo"));
        assert!(is_ado_url("https://user@dev.azure.com/org/proj/_git/repo"));
        assert!(!is_ado_url("https://github.com/org/repo.git"));
    }

    #[test]
    fn client_auth_header_format() {
        let client = AdoClient::new("https://dev.azure.com/org", "my-pat-token");
        let expected = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(":my-pat-token")
        );
        assert_eq!(client.auth_header, expected);
    }

    #[test]
    fn merge_strategy_strings() {
        assert_eq!(MergeStrategy::NoFastForward.as_str(), "noFastForward");
        assert_eq!(MergeStrategy::Squash.as_str(), "squash");
        assert_eq!(MergeStrategy::Rebase.as_str(), "rebase");
        assert_eq!(MergeStrategy::RebaseMerge.as_str(), "rebaseMerge");
    }
}
