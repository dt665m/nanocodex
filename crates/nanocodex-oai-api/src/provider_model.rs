//! Provider-owned metadata registration; no provider behavior lives here.
use crate::{Model, Thinking};
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use std::sync::{LazyLock, Mutex};

/// Immutable metadata supplied by an external provider crate.
#[derive(Debug, Eq, PartialEq)]
pub struct ProviderModel {
    /// Provider-native Responses model identifier.
    pub id: &'static str,
    /// Largest supported input context.
    pub context_window_tokens: u64,
    /// Default reasoning effort.
    pub default_thinking: Thinking,
    /// Supported efforts, as bits indexed by `Thinking as u8`.
    pub thinking_mask: u8,
    /// Whether the provider supports pro reasoning mode.
    pub supports_pro: bool,
    /// Provider-owned default instructions.
    pub system_prompt: &'static str,
    /// Input, cached input, cache-write input and output nano-USD per token.
    pub token_rates: [u64; 4],
}
static MODELS: LazyLock<Mutex<Vec<&'static ProviderModel>>> = LazyLock::new(Mutex::default);

/// Registers static metadata so model IDs in retained snapshots can be restored.
///
/// # Errors
/// Rejects an empty identifier or conflicting definitions for the same identifier.
pub fn register_provider_model(model: &'static ProviderModel) -> Result<(), String> {
    if model.id.trim().is_empty() {
        return Err("provider model ID must not be empty".into());
    }
    let mut models = MODELS
        .lock()
        .map_err(|_| "provider model registry unavailable")?;
    if let Some(existing) = models.iter().find(|existing| existing.id == model.id) {
        if *existing != model {
            return Err(format!("conflicting provider model {}", model.id));
        }
    } else {
        models.push(model);
    }
    Ok(())
}
pub(super) fn lookup(id: &str) -> Option<&'static ProviderModel> {
    MODELS
        .lock()
        .ok()?
        .iter()
        .copied()
        .find(|model| model.id == id)
}
impl Serialize for Model {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(match self {
            Self::Sol => "sol",
            Self::Luna => "luna",
            Self::Astra => "astra",
            Self::Glm53 => "glm-5.3",
            Self::Kimi => "kimi-k3",
            Self::Mimo => "mimo-v2.6-pro",
            Self::External(model) => model.id,
        })
    }
}
impl<'de> Deserialize<'de> for Model {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
}
