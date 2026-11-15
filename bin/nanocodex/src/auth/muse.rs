//! Host-owned Muse subscription storage; Meta's device flow lives in the provider.
use std::{io::Read, path::PathBuf, sync::Arc};

use clap::Args;
use eyre::{Result, eyre};
use nanocodex::{
    Muse,
    muse::auth::{MuseAuth, MuseAuthConfig, MuseCredential, MuseLogin},
    oai::auth::{
        OpenAiAuth, OpenAiAuthError, OpenAiAuthFuture, OpenAiAuthSnapshot, OpenAiAuthSource,
    },
};
use serde_json::json;
use tokio::sync::Mutex;

use super::store::{NativeStore, StoreError};

const MAGIC: &[u8] = b"nanocodex-muse-auth-v1\0";
const MAX_CONFIG_BYTES: u64 = 128 * 1024;

#[derive(Args, Clone, Default)]
pub(crate) struct MuseAuthArgs {
    /// Private encrypted Muse subscription store (default: CODEX_HOME/muse/private/auth).
    #[arg(long, env = "NANOCODEX_MUSE_AUTH_FILE", global = true)]
    muse_auth_file: Option<PathBuf>,
    /// Public MuseAuthConfig JSON for explicit native OAuth deployments.
    #[arg(long, global = true)]
    muse_oauth_config: Option<PathBuf>,
    /// Meta inference API key instead of subscription credentials.
    #[arg(
        long,
        env = "META_MODEL_API_KEY",
        hide_env_values = true,
        global = true
    )]
    muse_api_key: Option<String>,
    /// Override Muse's HTTP Responses base URL.
    #[arg(long, env = "NANOCODEX_MUSE_API_BASE_URL", global = true)]
    muse_api_base_url: Option<String>,
}

impl MuseAuthArgs {
    pub(crate) fn has_saved_credentials(&self) -> bool {
        self.path().is_ok_and(|path| path.is_file())
    }

    pub(crate) fn has_api_key(&self) -> bool {
        self.api_key().is_some()
    }

    fn api_key(&self) -> Option<String> {
        self.muse_api_key.clone().or_else(|| {
            ["MODEL_API_KEY", "META_API_KEY"]
                .into_iter()
                .find_map(|name| std::env::var(name).ok())
        })
    }

    fn path(&self) -> Result<PathBuf> {
        match &self.muse_auth_file {
            Some(path) if !path.as_os_str().is_empty() => Ok(path.clone()),
            Some(_) => eyre::bail!("--muse-auth-file must not be empty"),
            None => Ok(crate::config::default_codex_home()?.join("muse/private/auth")),
        }
    }

    fn store(&self) -> Result<NativeStore> {
        Ok(NativeStore::new(self.path()?, MAGIC))
    }

    fn config(&self) -> Result<MuseAuthConfig> {
        let Some(path) = &self.muse_oauth_config else {
            return Ok(MuseAuthConfig::default());
        };
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .and_then(|file| file.take(MAX_CONFIG_BYTES + 1).read_to_end(&mut bytes))
            .map_err(|_| eyre!("could not read public Muse OAuth configuration"))?;
        if bytes.len() as u64 > MAX_CONFIG_BYTES {
            eyre::bail!("public Muse OAuth configuration is too large");
        }
        serde_json::from_slice(&bytes)
            .map_err(|_| eyre!("invalid public Muse OAuth configuration JSON"))
    }

    pub(crate) async fn client(self) -> Result<Muse> {
        let config = self.config()?;
        let authorization = match self.api_key() {
            Some(api_key) => MuseAuth::with_config(
                MuseCredential {
                    access_token: None,
                    api_key,
                },
                &config,
            )?
            .authorization(),
            None => {
                let store = self.store()?;
                let (revision, payload) = load(store.clone()).await?;
                let payload = payload
                    .ok_or_else(|| eyre!("Muse login required; run `ncl --muse auth login`"))?;
                let credential: MuseCredential = serde_json::from_str(&payload).map_err(|_| {
                    eyre!("invalid private Muse credentials; run `ncl --muse auth login`")
                })?;
                let manager = MuseAuth::with_config(credential, &config)?;
                OpenAiAuth::managed_api_key(Arc::new(PersistedAuth {
                    authorization: manager.authorization(),
                    manager,
                    store,
                    revision: Mutex::new(revision),
                }))
            }
        };
        let mut builder = Muse::builder(authorization);
        if let Some(endpoint) = self.muse_api_base_url {
            builder = builder.api_base_url(endpoint);
        }
        Ok(builder.build()?)
    }

    pub(super) async fn login(self, open_automatically: bool) -> Result<()> {
        let store = self.store()?;
        let (revision, _) = load(store.clone()).await?;
        let login = MuseLogin::start_with_config(&self.config()?).await?;
        eprintln!(
            "Open this URL to sign in with Muse:\n\n{}\n\nDevice code: {}\n",
            login.verification_url(),
            login.user_code()
        );
        if open_automatically && super::open_browser(login.verification_url()).is_err() {
            eprintln!("Could not open a browser automatically. Open the URL above manually.");
        }
        let credentials = login.complete().await?;
        save(store, revision, Some(credentials)).await?;
        println!(
            "Logged in with Muse. Credentials saved to {}.",
            self.path()?.display()
        );
        Ok(())
    }

    pub(super) async fn status(self) -> Result<()> {
        let (_, payload) = load(self.store()?).await?;
        let oauth = if let Some(payload) = &payload {
            let credential: MuseCredential = serde_json::from_str(payload)
                .map_err(|_| eyre!("invalid private Muse credentials"))?;
            let has_oauth = credential.access_token.is_some();
            MuseAuth::with_config(credential, &self.config()?)?;
            has_oauth
        } else {
            false
        };
        println!(
            "{}",
            json!({"credentials_present":payload.is_some(), "oauth":oauth})
        );
        Ok(())
    }

    pub(super) async fn logout(self) -> Result<()> {
        let store = self.store()?;
        let (revision, _) = load(store.clone()).await?;
        save(store, revision, None).await?;
        println!("Logged out of Muse subscription.");
        Ok(())
    }
}

async fn load(store: NativeStore) -> Result<(u64, Option<String>)> {
    tokio::task::spawn_blocking(move || {
        store.with_store(|stored| Ok((stored.revision, stored.payload.clone())))
    })
    .await
    .map_err(|_| eyre!("Muse credential host failed"))?
    .map_err(|_| eyre!("could not load private Muse credentials"))
}

async fn save(
    store: NativeStore,
    revision: u64,
    credentials: Option<MuseCredential>,
) -> Result<u64> {
    let payload = credentials
        .map(|credentials| serde_json::to_string(&credentials))
        .transpose()
        .map_err(|_| eyre!("could not encode private Muse credentials"))?;
    tokio::task::spawn_blocking(move || {
        store.with_store(|stored| {
            // An active request must not resurrect a logged-out or replaced identity.
            if stored.revision != revision {
                return Err(StoreError);
            }
            stored.revision = stored.revision.checked_add(1).ok_or(StoreError)?;
            stored.payload = payload;
            Ok(stored.revision)
        })
    })
    .await
    .map_err(|_| eyre!("Muse credential host failed"))?
    .map_err(|_| eyre!("could not save private Muse credentials; the store may have changed"))
}

struct PersistedAuth {
    manager: MuseAuth,
    authorization: OpenAiAuth,
    store: NativeStore,
    revision: Mutex<u64>,
}

impl OpenAiAuthSource for PersistedAuth {
    fn validate(&self) -> std::result::Result<(), OpenAiAuthError> {
        self.authorization.validate()
    }
    fn snapshot(
        &self,
    ) -> OpenAiAuthFuture<'_, std::result::Result<OpenAiAuthSnapshot, OpenAiAuthError>> {
        Box::pin(async move {
            let revision = self.revision.lock().await;
            let (stored_revision, _) = load(self.store.clone())
                .await
                .map_err(|_| OpenAiAuthError::Provider("Muse credential storage failed".into()))?;
            if stored_revision != *revision {
                return Err(OpenAiAuthError::Provider(
                    "Muse credentials changed; reload credentials before retrying".into(),
                ));
            }
            self.authorization.snapshot().await
        })
    }
    fn recover_unauthorized(
        &self,
        rejected: &OpenAiAuthSnapshot,
    ) -> OpenAiAuthFuture<'_, std::result::Result<(), OpenAiAuthError>> {
        let rejected = rejected.clone();
        Box::pin(async move {
            let mut revision = self.revision.lock().await;
            self.authorization.recover_unauthorized(&rejected).await?;
            *revision = save(
                self.store.clone(),
                *revision,
                Some(self.manager.credentials().await),
            )
            .await
            .map_err(|_| {
                OpenAiAuthError::Provider(
                    "Muse credential storage failed; reload credentials before retrying".into(),
                )
            })?;
            Ok(())
        })
    }
}
