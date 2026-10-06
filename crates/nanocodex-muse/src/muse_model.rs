//! Muse-owned selectors and static provider metadata.
use nanocodex_oai_api::{Model, ProviderModel, Thinking};
use serde::{Deserialize, Serialize};
/// Muse Spark subscription variant.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum MuseModel {
    /// Standard Spark 1.3.
    #[default]
    #[serde(rename = "muse-spark-1.3")]
    Spark,
    /// Subsidized Spark: Meta may train on prompts and completions.
    #[serde(rename = "muse-spark-1.3-contributor")]
    Contributor,
}
const PROMPT: &str = include_str!("../prompts/muse.md");
static SPARK: ProviderModel = ProviderModel {
    id: "muse-spark-1.3",
    context_window_tokens: 1_048_576,
    default_thinking: Thinking::Low,
    thinking_mask: 0b111111,
    supports_pro: false,
    system_prompt: PROMPT,
    token_rates: [1250, 150, 1250, 4250],
};
static CONTRIBUTOR: ProviderModel = ProviderModel {
    id: "muse-spark-1.3-contributor",
    context_window_tokens: 1_048_576,
    default_thinking: Thinking::Low,
    thinking_mask: 0b011111,
    supports_pro: false,
    system_prompt: PROMPT,
    token_rates: [100, 2, 100, 200],
};
impl MuseModel {
    /// Provider-native model ID.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Spark => SPARK.id,
            Self::Contributor => CONTRIBUTOR.id,
        }
    }
}
impl From<MuseModel> for Model {
    fn from(model: MuseModel) -> Self {
        register();
        match model {
            MuseModel::Spark => Self::External(&SPARK),
            MuseModel::Contributor => Self::External(&CONTRIBUTOR),
        }
    }
}
pub(crate) fn register() {
    nanocodex_oai_api::register_provider_model(&SPARK).expect("static Muse model definition");
    nanocodex_oai_api::register_provider_model(&CONTRIBUTOR)
        .expect("static Muse contributor definition");
}
#[cfg(feature = "openai")]
pub(crate) fn is_muse(model: Model) -> bool {
    model == Model::External(&SPARK) || model == Model::External(&CONTRIBUTOR)
}

impl std::fmt::Display for MuseModel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}
impl std::str::FromStr for MuseModel {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "muse-spark-1.3" | "muse" => Ok(Self::Spark),
            "muse-spark-1.3-contributor" => Ok(Self::Contributor),
            _ => Err(format!(
                "invalid Muse model {value:?}; expected muse-spark-1.3 or muse-spark-1.3-contributor"
            )),
        }
    }
}
