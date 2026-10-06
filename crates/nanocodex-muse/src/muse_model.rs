//! Convenience selectors for the shared Muse model identifiers.
use nanocodex_oai_api::Model;
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
impl MuseModel {
    /// Provider-native model ID.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Spark => Model::MuseSpark13.as_str(),
            Self::Contributor => Model::MuseSpark13Contributor.as_str(),
        }
    }
}
impl From<MuseModel> for Model {
    fn from(model: MuseModel) -> Self {
        match model {
            MuseModel::Spark => Self::MuseSpark13,
            MuseModel::Contributor => Self::MuseSpark13Contributor,
        }
    }
}
#[cfg(feature = "openai")]
pub(crate) fn is_muse(model: Model) -> bool {
    matches!(model, Model::MuseSpark13 | Model::MuseSpark13Contributor)
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
