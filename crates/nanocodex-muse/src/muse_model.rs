//! Convenience selectors for the shared Muse model identifiers.
use nanocodex_oai_api::Model;
use serde::{Deserialize, Serialize};
/// Muse model selector.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(into = "Model", try_from = "Model")]
pub enum MuseModel {
    /// Standard Spark 1.3.
    #[default]
    Spark,
    /// Subsidized Spark: Meta may train on prompts and completions.
    Contributor,
    /// Wonder 1.0.
    Wonder,
    /// Wonder Full 1.0.
    WonderFull,
}
impl MuseModel {
    /// Provider-native model ID.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Spark => Model::MuseSpark13.as_str(),
            Self::Contributor => Model::MuseSpark13Contributor.as_str(),
            Self::Wonder => Model::MuseWonder10.as_str(),
            Self::WonderFull => Model::MuseWonderFull10.as_str(),
        }
    }
}
impl From<MuseModel> for Model {
    fn from(model: MuseModel) -> Self {
        match model {
            MuseModel::Spark => Self::MuseSpark13,
            MuseModel::Contributor => Self::MuseSpark13Contributor,
            MuseModel::Wonder => Self::MuseWonder10,
            MuseModel::WonderFull => Self::MuseWonderFull10,
        }
    }
}
impl TryFrom<Model> for MuseModel {
    type Error = String;
    fn try_from(model: Model) -> Result<Self, Self::Error> {
        match model {
            Model::MuseSpark13 => Ok(Self::Spark),
            Model::MuseSpark13Contributor => Ok(Self::Contributor),
            Model::MuseWonder10 => Ok(Self::Wonder),
            Model::MuseWonderFull10 => Ok(Self::WonderFull),
            other => Err(invalid_model(other.as_str())),
        }
    }
}

fn invalid_model(value: &str) -> String {
    format!(
        "invalid Muse model {value:?}; expected muse-spark-1.3, muse-spark-1.3-contributor, muse-wonder-1.0, or muse-wonder-full-1.0"
    )
}

impl std::fmt::Display for MuseModel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}
impl std::str::FromStr for MuseModel {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value
            .parse::<Model>()
            .map_err(|_| invalid_model(value))
            .and_then(Self::try_from)
    }
}
