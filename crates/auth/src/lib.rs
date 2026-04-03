use serde::{Deserialize, Serialize};
use std::process::Output;
use thiserror::Error;
use tokio::process::Command;

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("claude CLI not found in PATH")]
    CliNotFound,
    #[error("failed to execute claude CLI: {0}")]
    ExecFailed(#[from] std::io::Error),
    #[error("failed to parse CLI output: {0}")]
    ParseFailed(#[from] serde_json::Error),
}

/// Raw JSON returned by `claude auth status --json`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CliAuthStatus {
    logged_in: bool,
    auth_method: Option<String>,
    email: Option<String>,
    org_name: Option<String>,
    subscription_type: Option<String>,
}

/// Authenticated user info.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuthInfo {
    pub email: String,
    pub org_name: String,
    pub subscription_type: String,
    pub auth_method: String,
}

/// Result of the auth check.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status")]
pub enum AuthState {
    Ready(AuthInfo),
    NeedsLogin,
    NotInstalled,
}

impl std::fmt::Display for AuthState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ready(info) => {
                write!(f, "Ready — {} ({})", info.email, info.subscription_type)
            }
            Self::NeedsLogin => write!(f, "NeedsLogin — run `claude login` to authenticate"),
            Self::NotInstalled => write!(f, "NotInstalled — claude CLI not found in PATH"),
        }
    }
}

/// Check Claude CLI authentication status.
///
/// This calls `claude auth status --json` and parses the result.
/// Works across macOS (Keychain), Linux (credentials file), and Windows
/// (Credential Manager) — the CLI handles platform-specific storage internally.
pub async fn check_claude_auth() -> Result<AuthState, AuthError> {
    let output = run_cli().await?;
    parse_output(&output)
}

async fn run_cli() -> Result<Output, AuthError> {
    Command::new("claude")
        .args(["auth", "status", "--json"])
        .output()
        .await
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                AuthError::CliNotFound
            } else {
                AuthError::ExecFailed(e)
            }
        })
}

fn parse_output(output: &Output) -> Result<AuthState, AuthError> {
    if !output.status.success() {
        return Ok(AuthState::NotInstalled);
    }

    let status: CliAuthStatus = serde_json::from_slice(&output.stdout)?;

    if status.logged_in {
        Ok(AuthState::Ready(AuthInfo {
            email: status.email.unwrap_or_default(),
            org_name: status.org_name.unwrap_or_default(),
            subscription_type: status.subscription_type.unwrap_or_default(),
            auth_method: status.auth_method.unwrap_or_default(),
        }))
    } else {
        Ok(AuthState::NeedsLogin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;

    #[test]
    fn parse_ready() {
        let json = br#"{
            "loggedIn": true,
            "authMethod": "claude.ai",
            "apiProvider": "firstParty",
            "email": "user@example.com",
            "orgId": "abc-123",
            "orgName": "My Org",
            "subscriptionType": "max"
        }"#;

        let output = Output {
            status: ExitStatus::from_raw(0),
            stdout: json.to_vec(),
            stderr: vec![],
        };

        let state = parse_output(&output).unwrap();
        assert_eq!(
            state,
            AuthState::Ready(AuthInfo {
                email: "user@example.com".into(),
                org_name: "My Org".into(),
                subscription_type: "max".into(),
                auth_method: "claude.ai".into(),
            })
        );
    }

    #[test]
    fn parse_not_logged_in() {
        let json = br#"{"loggedIn": false}"#;

        let output = Output {
            status: ExitStatus::from_raw(0),
            stdout: json.to_vec(),
            stderr: vec![],
        };

        let state = parse_output(&output).unwrap();
        assert_eq!(state, AuthState::NeedsLogin);
    }

    #[test]
    fn parse_cli_failure() {
        let output = Output {
            status: ExitStatus::from_raw(1 << 8), // exit code 1
            stdout: vec![],
            stderr: b"error".to_vec(),
        };

        let state = parse_output(&output).unwrap();
        assert_eq!(state, AuthState::NotInstalled);
    }

    #[test]
    fn display_ready() {
        let state = AuthState::Ready(AuthInfo {
            email: "user@example.com".into(),
            org_name: "Org".into(),
            subscription_type: "team".into(),
            auth_method: "claude.ai".into(),
        });
        assert!(format!("{state}").contains("user@example.com"));
        assert!(format!("{state}").contains("team"));
    }
}
