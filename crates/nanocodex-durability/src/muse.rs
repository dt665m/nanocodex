//! Muse adapter for the shared Responses durable execution policy.

use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
};

use nanocodex_agent::{
    NanocodexError, Result as AgentResult, execution::ExecutionPolicy, session::SessionId,
};

use crate::{
    DurableAgentExt, DurableSession,
    agent::{Branches, DurableExecution, agent_error},
};

impl<F> DurableAgentExt for nanocodex_muse::NanocodexBuilder<F> {
    async fn durability(self, state: DurableSession) -> AgentResult<Self> {
        let state_id = state.state_id().to_owned();
        let record = state.record().await.map_err(agent_error)?;
        let mut builder = self.child_journal(state.child_journal());
        // A durable Muse session is identified by its state; events,
        // persistence, and resume all report the same identity. A host that
        // names the identity explicitly owns it: a Durable Object persisted
        // before identities were unified keeps the runtime session ID its
        // event log, task tree, effect journals and checkpoint lineage were
        // recorded under, rather than being silently renamed to its state ID.
        if builder.explicit_session_id().is_none()
            && let Ok(session_id) = state_id.parse::<SessionId>()
        {
            builder = builder.session_id(session_id);
        }
        if let Some(record) = &record {
            builder = builder.lineage(record.lineage.clone());
        }
        let initial = record
            .as_ref()
            .and_then(|record| match (record.model, record.initial) {
                (nanocodex_agent::HarnessModel::Muse(model), Some(initial)) => {
                    Some((model.into(), initial))
                }
                _ => None,
            });
        let branches = Branches {
            store: state.shared_store(),
            record,
        };
        let (owner, checkpoint) = state.acquire_agent().await.map_err(agent_error)?;
        let mut known_records = HashSet::new();
        if let Some(checkpoint) = checkpoint {
            let (restored, keys) = crate::context::load_snapshot_with_keys(
                (&owner).into(),
                checkpoint.decode().map_err(agent_error)?,
            )
            .await
            .map_err(agent_error)?;
            let restored: nanocodex_muse::session::SessionSnapshot = restored.try_into()?;
            if let Some(configured) = builder.resume_snapshot()
                && serde_json::to_string(configured)
                    .map_err(|error| NanocodexError::InvalidCheckpoint(error.to_string()))?
                    != serde_json::to_string(&restored)
                        .map_err(|error| NanocodexError::InvalidCheckpoint(error.to_string()))?
            {
                return Err(NanocodexError::InvalidCheckpoint(
                    "configured resume snapshot does not match the durability state".to_owned(),
                ));
            }
            known_records = keys;
            builder = builder.resume_native_snapshot(restored);
        } else if builder.resume_snapshot().is_none() {
            // A child reopened before its first checkpoint keeps the model and
            // settings it was created with.
            if let Some((model, initial)) = initial {
                builder = builder.initial_settings(model, initial.thinking, initial.service_tier);
            }
            // A fork's explicitly supplied completed snapshot owns its cache
            // lineage. A fresh durable root alone defaults to its state ID.
            builder = builder.default_prompt_cache_key(state_id.clone());
        }
        let owner = Arc::new(Mutex::new(Some((owner, known_records, branches))));
        Ok(builder
            .execution_policy_factory(move || {
                let (owner, keys, branches) = owner
                    .lock()
                    .map_err(|_| {
                        NanocodexError::InvalidExecutionPolicy(
                            "the durability-attached builder owner lock was poisoned".to_owned(),
                        )
                    })?
                    .take()
                    .ok_or_else(|| {
                        NanocodexError::InvalidExecutionPolicy(
                            "a durability-attached builder can build only one agent; attach durability again to reopen the state"
                                .to_owned(),
                        )
                    })?;
                let policy = DurableExecution::ready(owner, state_id.clone(), Some(branches));
                policy.remember(keys)?;
                let policy: Arc<dyn ExecutionPolicy> = Arc::new(policy);
                Ok(policy)
            }) )
    }
}
