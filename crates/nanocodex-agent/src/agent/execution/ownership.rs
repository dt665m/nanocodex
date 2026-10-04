use super::ExecutionFuture;
use crate::Result;

/// Embedding-owned foreground work that must settle before a turn is terminal.
/// The driver remains cancellable while waiting for successful completion.
pub trait TurnOwnership: Send + Sync + 'static {
    /// Waits for foreground work on success, or stops it before failure/abort.
    fn settle<'a>(&'a self, session_id: &'a str, success: bool) -> ExecutionFuture<'a, Result<()>>;
}
