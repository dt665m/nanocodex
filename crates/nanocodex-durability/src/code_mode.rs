//! Native cell journal uses the authoritative model owner's fenced step actor.
use std::{collections::HashMap, sync::{Arc, Mutex}};
use nanocodex_oai_tools::code_mode::{CodeJournalAdmission, CodeModeJournal};
use serde_json::{Value, json};
use crate::{BeginStep, DocumentForkPolicy, DocumentWrite, DurableSession, Error, ReplaySafety, session::DurableOwner};

const STORE_KEY: &str = "nanocodex.code-mode.store";
#[derive(Clone)]
struct Scope { operation: String, step: String }

pub(crate) struct DurableCodeJournal {
    owner: Arc<DurableOwner>,
    state: DurableSession,
    scopes: Mutex<HashMap<String, Scope>>,
    active: Mutex<HashMap<String, Scope>>,
}
impl DurableCodeJournal {
    pub(crate) fn new(owner: Arc<DurableOwner>, state: DurableSession) -> Self {
        Self { owner, state, scopes: Mutex::default(), active: Mutex::default() }
    }
    pub(crate) fn bind(&self, operation: &str, step: &str, input: &str) -> crate::Result<()> {
        let value: Value = serde_json::from_str(input).map_err(Error::InvalidPayload)?;
        if value.get("name").and_then(Value::as_str) != Some("exec") { return Ok(()); }
        let call_id = value.get("call_id").and_then(Value::as_str).ok_or_else(|| Error::InvalidState("Code Mode tool identity is missing".into()))?;
        let mut scopes = self.scopes.lock().map_err(|_| Error::InvalidState("Code Mode scope lock poisoned".into()))?;
        // Keep only live call scopes; the durable steps own historical identity.
        let active = self.active.lock().map_err(|_| Error::InvalidState("Code Mode active lock poisoned".into()))?;
        if active.contains_key(call_id) { return Err(Error::InvalidState("Code Mode call identity is still active; execution outcome unknown".into())); }
        if scopes.len() >= 64 && !scopes.contains_key(call_id) { return Err(Error::InvalidState("Code Mode admission queue exceeds 64 cells".into())); }
        scopes.insert(call_id.into(), Scope { operation: operation.into(), step: format!("code-cell:{step}") });
        Ok(())
    }
    fn scope(&self, call: &str, active: bool) -> Result<Scope, String> {
        let map = if active { &self.active } else { &self.scopes };
        map.lock().map_err(|_| "Code Mode scope lock poisoned")?.get(call).cloned().ok_or_else(|| "Code Mode operation scope missing; execution outcome unknown".into())
    }
}
#[async_trait::async_trait]
impl CodeModeJournal for DurableCodeJournal {
    async fn admit_cell(&self, session_id: &str, call_id: &str, source: &str) -> Result<CodeJournalAdmission, String> {
        let scope = self.scopes.lock().map_err(|_| "Code Mode scope lock poisoned")?.remove(call_id).ok_or("Code Mode operation scope missing; execution outcome unknown")?;
        let admission = self.owner.begin_step(scope.operation.clone(), scope.step.clone(), "code_cell".into(), &json!({"session_id":session_id,"source":source}), ReplaySafety::Unsafe).await.map_err(|e| e.to_string())?;
        match admission {
            BeginStep::OutcomeUnknown => Ok(CodeJournalAdmission::Unknown),
            BeginStep::Replay(output) => Ok(CodeJournalAdmission::Replay(output.decode().map_err(|e| e.to_string())?)),
            BeginStep::Execute => {
                let document = self.state.document(STORE_KEY).await.map_err(|e| e.to_string())?;
                let (stored, version) = match document {
                    Some(doc) => (serde_json::from_value(doc.value).map_err(|e| format!("Code Mode store corrupt: {e}"))?, doc.version),
                    None => (HashMap::new(), 0),
                };
                self.active.lock().map_err(|_| "Code Mode active lock poisoned")?.insert(call_id.into(), scope);
                Ok(CodeJournalAdmission::Execute { stored, version })
            }
        }
    }
    async fn begin_effect(&self, call_id: &str, effect_id: &str, name: &str, input: &Value) -> Result<(), String> {
        let scope = self.scope(call_id, true)?;
        match self.owner.begin_step(scope.operation, format!("{}/effect:{effect_id}",scope.step), "code_effect".into(), &json!({"name":name,"input":input}), ReplaySafety::Unsafe).await.map_err(|e| e.to_string())? {
            BeginStep::Execute => Ok(()),
            _ => Err("Code Mode nested effect already admitted; execution outcome unknown".into()),
        }
    }
    async fn complete_effect(&self, call_id: &str, effect_id: &str, receipt: &Value) -> Result<(), String> {
        let scope = self.scope(call_id, true)?;
        self.owner.complete_step(scope.operation, format!("{}/effect:{effect_id}",scope.step), receipt).await.map_err(|e| e.to_string())
    }
    async fn complete_cell(&self, call_id: &str, expected_version: u64, stored: Option<HashMap<String, Value>>, receipt: &Value) -> Result<(), String> {
        let scope = self.scope(call_id, true)?;
        let writes = match stored {
            Some(stored) => vec![DocumentWrite { key: STORE_KEY.into(), expected_version, value: serde_json::to_value(stored).map_err(|e| e.to_string())?, fork: DocumentForkPolicy::AsOf }],
            None => vec![],
        };
        self.owner.complete_code_cell(scope.operation, scope.step, receipt, writes).await.map_err(|e| e.to_string())?;
        self.active.lock().map_err(|_| "Code Mode active lock poisoned")?.remove(call_id);
        Ok(())
    }
}
