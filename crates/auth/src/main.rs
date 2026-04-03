use nabla_auth::{check_claude_auth, AuthState};

#[tokio::main]
async fn main() {
    match check_claude_auth().await {
        Ok(AuthState::Ready(info)) => {
            println!("Claude CLI authenticated");
            println!("  Email:        {}", info.email);
            println!("  Organization: {}", info.org_name);
            println!("  Subscription: {}", info.subscription_type);
            println!("  Auth method:  {}", info.auth_method);
        }
        Ok(AuthState::NeedsLogin) => {
            eprintln!("Claude CLI is installed but not logged in.");
            eprintln!("Run: claude login");
            std::process::exit(1);
        }
        Ok(AuthState::NotInstalled) => {
            eprintln!("Claude CLI not found or returned an error.");
            eprintln!("Install: npm install -g @anthropic-ai/claude-code");
            std::process::exit(2);
        }
        Err(e) => {
            eprintln!("Error: {e}");
            std::process::exit(3);
        }
    }
}
