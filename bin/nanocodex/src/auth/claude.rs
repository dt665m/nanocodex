//! Native, private host capabilities for Rust-owned Claude subscription OAuth.
use std::{
    fs::File,
    io::{IsTerminal, Read},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use super::store::{NativeStore, PrivateStore};
use clap::Args;
use eyre::{Result, eyre};
use futures_util::StreamExt;
use nanocodex::claude::{
    ClaudeAuthFuture, ClaudeAuthProvider, ClaudeClient, SubscriptionIdentity,
    subscription::{
        ClaudeLoginMode, ClaudeSubscription, ClaudeSubscriptionCommit, ClaudeSubscriptionConfig,
        ClaudeSubscriptionHost, ClaudeSubscriptionHostError, ClaudeSubscriptionHttpRequest,
        ClaudeSubscriptionHttpResponse, ClaudeSubscriptionStatus, ClaudeSubscriptionStoreValue,
    },
};

const STORE_KEY: &str = "claude-subscription";
const MAGIC: &[u8] = b"nanocodex-claude-auth-v1\0";
const MAX_STORE_BYTES: u64 = 128 * 1024;
type HostResult<T> = std::result::Result<T, ClaudeSubscriptionHostError>;

#[derive(Args, Clone, Default)]
pub(crate) struct ClaudeAuthArgs {
    /// Private encrypted Claude subscription store (default: CODEX_HOME/claude/private/auth).
    #[arg(long, env = "NANOCODEX_CLAUDE_AUTH_FILE", global = true)]
    claude_auth_file: Option<PathBuf>,
    /// Public ClaudeSubscriptionConfig JSON for explicit OAuth deployments.
    #[arg(long, global = true)]
    claude_oauth_config: Option<PathBuf>,
}

impl ClaudeAuthArgs {
    pub(crate) async fn client(
        self,
        api_key: Option<String>,
        messages_url: Option<String>,
    ) -> Result<ClaudeClient> {
        if let Some(api_key) = api_key {
            if api_key.trim().is_empty() {
                eyre::bail!("Claude API key must not be empty");
            }
            return Ok(ClaudeClient::new(
                reqwest::Client::new(),
                messages_url.unwrap_or_else(|| nanocodex::claude::ANTHROPIC_MESSAGES_URL.into()),
                api_key,
            ));
        }
        let (host, manager) = self.manager()?;
        let mut status = manager
            .status()
            .await
            .map_err(|error| eyre!("{error}; run `nanocodex --claude auth login` to sign in"))?;
        if status == ClaudeSubscriptionStatus::Validating {
            // A prior process may have persisted a rotated token before profile
            // validation completed. Let the manager safely resume that phase.
            manager.headers().await.map_err(|_| {
                eyre!("Claude subscription validation failed; run `nanocodex --claude auth login` to recover")
            })?;
            status = manager.status().await?;
        }
        let ClaudeSubscriptionStatus::Authenticated { account_id, .. } = status else {
            eyre::bail!(
                "Claude subscription is not authenticated; run `nanocodex --claude auth login`"
            );
        };
        let identity = SubscriptionIdentity {
            install_id: Some(host.install_id().await?),
            account_uuid: Some(account_id),
            ..SubscriptionIdentity::default()
        };
        let provider = Arc::new(manager);
        let http = subscription_http()?;
        let client = match messages_url {
            Some(endpoint) => ClaudeClient::with_auth_provider(http, endpoint, provider)
                .subscription_compatibility(),
            None => ClaudeClient::subscription(http, provider),
        };
        Ok(client.with_subscription_identity(identity))
    }

    /// Whether a local Claude subscription store exists; validity is checked on use.
    pub(crate) fn has_saved_credentials(&self) -> bool {
        match &self.claude_auth_file {
            Some(path) => !path.as_os_str().is_empty() && path.is_file(),
            None => crate::config::default_codex_home()
                .is_ok_and(|home| home.join("claude/private/auth").is_file()),
        }
    }

    fn manager(self) -> Result<(Arc<NativeHost>, ClaudeSubscription)> {
        let path = match self.claude_auth_file {
            Some(path) if !path.as_os_str().is_empty() => path,
            Some(_) => eyre::bail!("--claude-auth-file must not be empty"),
            None => crate::config::default_codex_home()?.join("claude/private/auth"),
        };
        let config = self
            .claude_oauth_config
            .as_deref()
            .map(read_public_config)
            .transpose()?
            .unwrap_or_default();
        let host = Arc::new(NativeHost {
            path,
            http: subscription_http()?,
        });
        let manager = ClaudeSubscription::new(host.clone(), STORE_KEY, config)?;
        Ok((host, manager))
    }

    pub(super) async fn login(self, open_automatically: bool) -> Result<()> {
        let (_, manager) = self.manager()?;
        let login = manager.begin_login(ClaudeLoginMode::Manual).await?;
        eprintln!(
            "Open this URL to sign in with Claude:\n\n{}\n",
            login.authorization_url
        );
        if open_automatically && super::open_browser(&login.authorization_url).is_err() {
            eprintln!("Could not open a browser automatically. Open the URL above manually.");
        }
        eprintln!("Paste the returned code#state and press Enter (input is hidden):");
        let code = tokio::task::spawn_blocking(read_login_code)
            .await
            .map_err(|_| eyre!("could not read Claude login response"))??;
        let status = manager.complete_login(&code).await?;
        println!("{}", serde_json::to_string(&status)?);
        Ok(())
    }

    pub(super) async fn status(self) -> Result<()> {
        let (_, manager) = self.manager()?;
        println!("{}", serde_json::to_string(&manager.status().await?)?);
        Ok(())
    }

    pub(super) async fn logout(self) -> Result<()> {
        let (_, manager) = self.manager()?;
        manager.logout().await?;
        println!("Logged out of Claude subscription.");
        Ok(())
    }
}

fn subscription_http() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .build()
        .map_err(|_| eyre!("could not initialize Claude subscription HTTP host"))
}

fn read_public_config(path: &Path) -> Result<ClaudeSubscriptionConfig> {
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take(MAX_STORE_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|_| eyre!("could not read public Claude OAuth configuration"))?;
    if bytes.len() as u64 > MAX_STORE_BYTES {
        eyre::bail!("public Claude OAuth configuration is too large");
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| eyre!("invalid public Claude OAuth configuration JSON"))
}

fn read_login_code() -> Result<String> {
    const MAX_CODE: usize = 8192;
    let mut value = String::new();
    if std::io::stdin().is_terminal() {
        use crossterm::{
            event::{self, Event, KeyCode, KeyModifiers},
            terminal,
        };
        struct RestoreTerminal;
        impl Drop for RestoreTerminal {
            fn drop(&mut self) {
                let _ = terminal::disable_raw_mode();
            }
        }
        terminal::enable_raw_mode().map_err(|_| eyre!("could not hide Claude login input"))?;
        let _restore = RestoreTerminal;
        loop {
            match event::read().map_err(|_| eyre!("could not read Claude login input"))? {
                Event::Key(key) if key.kind != event::KeyEventKind::Release => match key.code {
                    KeyCode::Enter => break,
                    KeyCode::Char('c' | 'd') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        eyre::bail!("Claude login input cancelled");
                    }
                    KeyCode::Backspace => {
                        value.pop();
                    }
                    KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        value.push(character)
                    }
                    _ => {}
                },
                Event::Paste(paste) => value.push_str(&paste),
                _ => {}
            }
            if value.len() > MAX_CODE {
                eyre::bail!("Claude login input is too large");
            }
        }
        eprintln!();
    } else {
        let mut input = std::io::stdin().lock();
        let mut bytes = Vec::new();
        loop {
            let mut byte = [0];
            match input.read(&mut byte) {
                Ok(0) => break,
                Ok(_) if byte[0] == b'\n' => break,
                Ok(_) => bytes.push(byte[0]),
                Err(_) => eyre::bail!("could not read Claude login input"),
            }
            if bytes.len() > MAX_CODE {
                eyre::bail!("Claude login input is too large");
            }
        }
        value = String::from_utf8(bytes).map_err(|_| eyre!("invalid Claude login input"))?;
    }
    Ok(value.trim().to_owned())
}

#[derive(Clone)]
struct NativeHost {
    path: PathBuf,
    http: reqwest::Client,
}

impl NativeHost {
    async fn install_id(&self) -> Result<String> {
        let host = self.clone();
        tokio::task::spawn_blocking(move || host.with_store(|store| Ok(store.install_id.clone())))
            .await
            .map_err(|_| eyre!("Claude subscription host failed"))?
            .map_err(Into::into)
    }

    fn with_store<T>(
        &self,
        action: impl FnOnce(&mut PrivateStore) -> HostResult<T>,
    ) -> HostResult<T> {
        NativeStore::new(self.path.clone(), MAGIC)
            .with_store(|store| action(store).map_err(|_| super::store::StoreError))
            .map_err(|_| ClaudeSubscriptionHostError)
    }
}

impl ClaudeSubscriptionHost for NativeHost {
    fn load<'a>(
        &'a self,
        key: &'a str,
    ) -> ClaudeAuthFuture<'a, HostResult<ClaudeSubscriptionStoreValue>> {
        Box::pin(async move {
            if key != STORE_KEY {
                return Err(ClaudeSubscriptionHostError);
            }
            let host = self.clone();
            tokio::task::spawn_blocking(move || {
                host.with_store(|store| {
                    Ok(ClaudeSubscriptionStoreValue {
                        revision: store.revision,
                        payload: store.payload.clone(),
                    })
                })
            })
            .await
            .map_err(|_| ClaudeSubscriptionHostError)?
        })
    }

    fn compare_and_swap<'a>(
        &'a self,
        key: &'a str,
        expected_revision: u64,
        payload: &'a str,
    ) -> ClaudeAuthFuture<'a, HostResult<ClaudeSubscriptionCommit>> {
        Box::pin(async move {
            if key != STORE_KEY || payload.len() > 64 * 1024 {
                return Err(ClaudeSubscriptionHostError);
            }
            let host = self.clone();
            let payload = payload.to_owned();
            tokio::task::spawn_blocking(move || {
                host.with_store(|store| {
                    if store.revision != expected_revision {
                        return Ok(ClaudeSubscriptionCommit::Conflict(store.revision));
                    }
                    store.revision = store
                        .revision
                        .checked_add(1)
                        .ok_or(ClaudeSubscriptionHostError)?;
                    store.payload = Some(payload);
                    Ok(ClaudeSubscriptionCommit::Committed(store.revision))
                })
            })
            .await
            .map_err(|_| ClaudeSubscriptionHostError)?
        })
    }

    fn request(
        &self,
        request: ClaudeSubscriptionHttpRequest,
    ) -> ClaudeAuthFuture<'_, HostResult<ClaudeSubscriptionHttpResponse>> {
        Box::pin(async move {
            let deadline = Duration::from_millis(request.timeout_millis());
            tokio::time::timeout(deadline, async {
                let method = reqwest::Method::from_bytes(request.method().as_bytes())
                    .map_err(|_| ClaudeSubscriptionHostError)?;
                let response = self
                    .http
                    .request(method, request.url())
                    .headers(request.headers().clone())
                    .header(reqwest::header::CONTENT_TYPE, request.content_type())
                    .body(request.body().to_owned())
                    .timeout(deadline)
                    .send()
                    .await
                    .map_err(|_| ClaudeSubscriptionHostError)?;
                let status = response.status().as_u16();
                let bound = request.max_response_bytes();
                if response
                    .content_length()
                    .is_some_and(|length| length > bound as u64)
                {
                    return Err(ClaudeSubscriptionHostError);
                }
                let mut stream = response.bytes_stream();
                let mut bytes = Vec::new();
                while let Some(chunk) = stream.next().await {
                    let chunk = chunk.map_err(|_| ClaudeSubscriptionHostError)?;
                    if chunk.len() > bound.saturating_sub(bytes.len()) {
                        return Err(ClaudeSubscriptionHostError);
                    }
                    bytes.extend_from_slice(&chunk);
                }
                let body = String::from_utf8(bytes).map_err(|_| ClaudeSubscriptionHostError)?;
                Ok(ClaudeSubscriptionHttpResponse { status, body })
            })
            .await
            .map_err(|_| ClaudeSubscriptionHostError)?
        })
    }
}
