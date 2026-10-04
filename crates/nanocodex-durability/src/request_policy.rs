//! Persisted prompt/tool configuration and request-scoped virtual routing.
//!
//! Configuration controls declaration rendering, never tool execution authority.
//! Provider messages and opaque signatures remain owned by the native harness.

use nanocodex_agent::{HarnessFamily, HarnessModel};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use crate::{Error, Result};

/// One named instruction section, ordered by first insertion (re-add appends).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PromptSection {
    /// Stable section key.
    pub name: String,
    /// Exact instruction text.
    pub text: String,
}

/// A named provider-native tool declaration. This does not authorize execution.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolDeclaration {
    /// Stable tool key.
    pub name: String,
    /// Provider-native schema, retained without translation.
    pub definition: Value,
}

/// A historical configuration change at a model-request boundary.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConfigurationPatch {
    /// Insert or replace a named section in its existing position.
    SetSection { /// Section to install.
        section: PromptSection },
    /// Remove a section; unknown keys are harmless.
    RemoveSection { /// Section key.
        name: String },
    /// Insert or replace a tool declaration in its existing position.
    SetTool { /// Declaration to install.
        tool: ToolDeclaration },
    /// Remove a declaration; unknown keys are harmless.
    RemoveTool { /// Tool key.
        name: String },
}

/// All patches admitted together for one request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConfigurationEntry {
    /// Stable host-derived request identity, not a provider response ID.
    pub request_id: String,
    /// Ordered changes.
    pub patches: Vec<ConfigurationPatch>,
}

/// Effective configuration at one historical boundary.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EffectiveConfiguration {
    /// Ordered named sections.
    pub sections: Vec<PromptSection>,
    /// Ordered native declarations.
    pub tools: Vec<ToolDeclaration>,
}

impl EffectiveConfiguration {
    /// Flatten sections for native adapters without positional patch support.
    pub fn instructions(&self) -> String {
        self.sections.iter().map(|s| s.text.as_str()).collect::<Vec<_>>().join("\n\n")
    }

    /// Reject declarations not present byte-for-JSON-value in the host catalog.
    /// The dispatcher must still perform its normal per-call authorization.
    pub fn authorize(&self, catalog: &[ToolDeclaration]) -> Result<()> {
        if self.tools.iter().any(|tool| !catalog.contains(tool)) {
            return Err(invalid("configuration includes an unauthorized tool declaration"));
        }
        Ok(())
    }

    fn patch(&mut self, patch: &ConfigurationPatch) -> Result<()> {
        match patch {
            ConfigurationPatch::SetSection { section } => {
                nonempty(&section.name)?;
                if let Some(old) = self.sections.iter_mut().find(|s| s.name == section.name) {
                    *old = section.clone();
                } else { self.sections.push(section.clone()); }
            }
            ConfigurationPatch::RemoveSection { name } => self.sections.retain(|s| &s.name != name),
            ConfigurationPatch::SetTool { tool } => {
                nonempty(&tool.name)?;
                if let Some(old) = self.tools.iter_mut().find(|t| t.name == tool.name) {
                    *old = tool.clone();
                } else { self.tools.push(tool.clone()); }
            }
            ConfigurationPatch::RemoveTool { name } => self.tools.retain(|t| &t.name != name),
        }
        Ok(())
    }
}

/// Append-only configuration history, serialized in the session document.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ConfigurationHistory {
    entries: Vec<ConfigurationEntry>,
}

impl ConfigurationHistory {
    /// Historical changes in admission order.
    pub fn entries(&self) -> &[ConfigurationEntry] { &self.entries }

    /// Replay through an inclusive boundary; `None` selects the latest.
    pub fn at(&self, request_id: Option<&str>) -> Result<EffectiveConfiguration> {
        let mut configuration = EffectiveConfiguration::default();
        for entry in &self.entries {
            for patch in &entry.patches { configuration.patch(patch)?; }
            if request_id == Some(entry.request_id.as_str()) { return Ok(configuration); }
        }
        if request_id.is_some() { return Err(invalid("unknown configuration boundary")); }
        Ok(configuration)
    }

    fn append(&mut self, entry: ConfigurationEntry) -> Result<()> {
        nonempty(&entry.request_id)?;
        if self.entries.iter().any(|old| old.request_id == entry.request_id) {
            return Err(invalid("duplicate configuration boundary"));
        }
        // Validate before admitting any partial patch.
        let mut next = self.at(None)?;
        for patch in &entry.patches { next.patch(patch)?; }
        self.entries.push(entry);
        Ok(())
    }
}

/// Host-approved physical model and limits; availability/credentials stay native.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PhysicalModel {
    /// Concrete native model identity.
    pub model: HarnessModel,
    /// Total context capacity, including reserved output.
    pub context_tokens: u64,
    /// Maximum output tokens.
    pub max_output_tokens: u64,
    /// Explicit native transcript compatibility group. `None` pins this model.
    /// Claude switches remain prohibited when an existing transcript is present.
    pub switch_group: Option<String>,
}

/// A new model request. Retries must reuse the entire value and request identity.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RouteRequest {
    /// Stable host-derived request identity.
    pub request_id: String,
    /// Public virtual model selected by the user.
    pub selection: String,
    /// Prior request whose unfinished tool/stream continuation this belongs to.
    pub continuation_of: Option<String>,
    /// Conservative measured/estimated input tokens for the rendered request.
    pub input_tokens: u64,
    /// Requested output reservation.
    pub output_tokens: u64,
    /// True only at a native boundary permitting a physical model change.
    /// Must be false with unresolved tool results or opaque continuation state.
    pub switch_safe: bool,
}

/// Router input. Policies should be deterministic and have no external effects.
pub struct RoutingInput<'a> {
    /// Request under consideration.
    pub request: &'a RouteRequest,
    /// Branch-local state from the preceding request.
    pub state: &'a Value,
    /// Last dispatched physical model.
    pub previous: Option<HarnessModel>,
    /// Host-approved candidates with physical limits.
    pub models: &'a [PhysicalModel],
}

/// Policy output committed before the provider is called.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RoutingChoice {
    /// Physical identity used for transport and usage attribution.
    pub dispatched: HarnessModel,
    /// Replacement branch-local policy state.
    pub state: Value,
}

/// Extension interface invoked once for each newly admitted non-continuation request.
pub trait VirtualModelRouter {
    /// Choose a physical model and the next persisted policy state.
    fn route(&self, input: RoutingInput<'_>) -> Result<RoutingChoice>;
}

impl<F> VirtualModelRouter for F where F: Fn(RoutingInput<'_>) -> Result<RoutingChoice> {
    fn route(&self, input: RoutingInput<'_>) -> Result<RoutingChoice> { self(input) }
}

/// Exact prepared request and routing receipt retained for retries and attribution.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PreparedRequest {
    /// Original request selection and limit inputs.
    pub request: RouteRequest,
    /// Physical model and router state after this decision.
    pub route: RoutingChoice,
    /// Exact serialized provider request before transport authentication.
    /// Credentials must never be included.
    pub request_json: String,
    /// Configuration rendered into this request.
    pub configuration: EffectiveConfiguration,
}

/// One branch's configuration, physical selections, and exact request checkpoints.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RequestPolicyState {
    /// Positional source history; current native adapters flatten it.
    pub configuration: ConfigurationHistory,
    /// Receipts in admission order.
    pub requests: Vec<PreparedRequest>,
}

impl RequestPolicyState {
    /// Prepare a request in memory. Persist the returned state before dispatch;
    /// use the durable request adapter for that boundary.
    /// `render` receives the final effective configuration and physical identity.
    pub fn prepare(
        &mut self,
        request: RouteRequest,
        patches: Vec<ConfigurationPatch>,
        models: &[PhysicalModel],
        authorized_tools: &[ToolDeclaration],
        router: &impl VirtualModelRouter,
        render: impl FnOnce(&EffectiveConfiguration, HarnessModel) -> Result<String>,
    ) -> Result<PreparedRequest> {
        nonempty(&request.request_id)?;
        nonempty(&request.selection)?;
        if let Some(saved) = self.requests.iter().find(|p| p.request.request_id == request.request_id) {
            let entry = self.configuration.entries.iter().find(|e| e.request_id == request.request_id);
            if saved.request != request || entry.is_none_or(|e| e.patches != patches) {
                return Err(invalid("request identity reused with different routing/configuration input"));
            }
            saved.configuration.authorize(authorized_tools)?;
            validate_limits(&request, saved.route.dispatched, models)?;
            return Ok(saved.clone());
        }
        let previous = self.requests.last();
        let state = previous.map_or(&Value::Null, |p| &p.route.state);
        let choice = if let Some(id) = &request.continuation_of {
            let predecessor = previous.filter(|p| &p.request.request_id == id)
                .ok_or_else(|| invalid("continuation must reference the latest request"))?;
            if request.selection != predecessor.request.selection {
                return Err(invalid("virtual selection cannot change during continuation"));
            }
            if !patches.is_empty() { return Err(invalid("configuration changes require a completed native boundary")); }
            predecessor.route.clone()
        } else {
            router.route(RoutingInput { request: &request, state,
                previous: previous.map(|p| p.route.dispatched), models })?
        };
        validate_limits(&request, choice.dispatched, models)?;
        if let Some(previous) = previous {
            let from = previous.route.dispatched;
            if from.family() != choice.dispatched.family() {
                return Err(invalid("cross-family transcript routing is unsupported"));
            }
            if from != choice.dispatched {
                let old = models.iter().find(|p| p.model == from);
                let new = models.iter().find(|p| p.model == choice.dispatched);
                let compatible = old.zip(new).is_some_and(|(a,b)|
                    a.switch_group.as_ref().is_some_and(|g| !g.is_empty() && Some(g) == b.switch_group.as_ref()));
                if !request.switch_safe || from.family() == HarnessFamily::Claude || !compatible {
                    return Err(invalid("native transcript does not permit this physical model switch"));
                }
            }
        }
        let mut history = self.configuration.clone();
        history.append(ConfigurationEntry { request_id: request.request_id.clone(), patches })?;
        let configuration = history.at(None)?;
        configuration.authorize(authorized_tools)?;
        let request_json = render(&configuration, choice.dispatched)?;
        serde_json::from_str::<Value>(&request_json)?;
        let prepared = PreparedRequest { request, route: choice, request_json, configuration };
        self.configuration = history;
        self.requests.push(prepared.clone());
        Ok(prepared)
    }
}

fn validate_limits(request: &RouteRequest, model: HarnessModel, models: &[PhysicalModel]) -> Result<()> {
    if models.iter().filter(|p| p.model == model).count() != 1 {
        return Err(invalid("physical model must occur exactly once in the approved catalog"));
    }
    let physical = models.iter().find(|p| p.model == model).expect("checked model");
    if request.output_tokens == 0 || request.output_tokens > physical.max_output_tokens
        || request.input_tokens.checked_add(request.output_tokens).is_none_or(|n| n > physical.context_tokens) {
        return Err(invalid("rendered request exceeds dispatched model limits"));
    }
    Ok(())
}

fn nonempty(value: &str) -> Result<()> {
    if value.is_empty() { Err(invalid("configuration and request identities must be nonempty")) } else { Ok(()) }
}
fn invalid(message: &str) -> Error { Error::InvalidState(message.into()) }
