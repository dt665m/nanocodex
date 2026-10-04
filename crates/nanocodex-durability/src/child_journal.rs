//! Fenced child-tree metadata on the same host store as durable execution.

use serde::{Serialize, de::DeserializeOwned};
use crate::{OwnerId, OwnerToken, StateStore, StoreError};

/// A child-tree journal. A failed write poisons this owner: reopen to reconcile
/// authoritative state before admitting further work or reporting receipts.
pub struct ChildJournal {
    store: Box<dyn StateStore>,
    state_id: String,
    owner: OwnerToken,
    revision: u64,
    payload: Option<String>,
    poisoned: bool,
}

impl ChildJournal {
    /// Acquires the host's existing ownership/revision fence for this tree.
    pub async fn open(store: impl StateStore + 'static, root: &str) -> crate::Result<Self> {
        let mut store = Box::new(store);
        let state_id = format!("{root}/children");
        let acquired = store.acquire(&state_id, OwnerId::new()).await?;
        Ok(Self { store, state_id, owner: acquired.owner,
            revision: acquired.state.revision, payload: acquired.state.payload,
            poisoned: false })
    }

    /// Decodes the last committed child tree without acquiring another owner.
    pub fn load<T: DeserializeOwned>(&self) -> crate::Result<Option<T>> {
        self.payload.as_deref().map(serde_json::from_str).transpose()
            .map_err(|source| crate::Error::Decode { revision: self.revision, source })
    }

    /// Commits a complete metadata transition before its observable receipt.
    pub async fn commit(&mut self, state: &impl Serialize) -> crate::Result<()> {
        if self.poisoned { return Err(StoreError::Fenced.into()); }
        let payload = serde_json::to_string(state)?;
        match self.store.replace(&self.state_id, &self.owner, self.revision, &payload, &[]).await {
            Ok(revision) => { self.revision = revision; self.payload = Some(payload); Ok(()) }
            Err(error) => { self.poisoned = true; Err(error.into()) }
        }
    }
}
