use crate::Registry;
use nanocodex_agent::{
    NanocodexError, Result,
    execution::{ExecutionFuture, TurnOwnership},
};
use std::sync::Arc;

/// Native lifecycle barrier for a registry's foreground ownership boundaries.
pub struct RegistryOwnership(pub Arc<Registry>);

impl TurnOwnership for RegistryOwnership {
    fn prepare<'a>(&'a self, session_id: &'a str) -> ExecutionFuture<'a, Result<()>> {
        Box::pin(async move {
            self.0
                .recover_registered(session_id)
                .await
                .map_err(|error| NanocodexError::InvalidExecutionPolicy(error.to_string()))
        })
    }

    fn settle<'a>(&'a self, session_id: &'a str, success: bool) -> ExecutionFuture<'a, Result<()>> {
        Box::pin(async move {
            let result = if success {
                self.0.wait_foreground(session_id).await
            } else {
                self.0.abort_foreground(session_id).await
            };
            result.map_err(|error| NanocodexError::InvalidExecutionPolicy(error.to_string()))
        })
    }
}
