//! Caller-owned tool lifecycle hooks. Hooks run inside the admitted tool effect;
//! committed durable receipts bypass them on replay. Hosts must reconcile hooks
//! with external effects using the supplied stable invocation identity.
use crate::{ClaudeToolInvocation, ClaudeToolReply};
use serde_json::Value;
use std::{future::Future, pin::Pin};

/// A hook future follows the target's transport threading contract.
#[cfg(not(target_family = "wasm"))]
pub type ClaudeHookFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;
#[cfg(target_family = "wasm")]
pub type ClaudeHookFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// The host decides whether a client tool may execute. Replacements are still
/// validated by that tool; they do not change its name or grant capabilities.
#[derive(Clone, Debug)]
pub enum ClaudeToolDecision {
    Allow,
    UpdateInput(Value),
    Deny(String),
}

/// Explicit host authorization and observation at the native client tool boundary.
/// No commands, settings, or permissions are loaded implicitly. Server tools run
/// at the provider and are outside this interface; enable them independently.
pub trait ClaudeToolHooks: Send + Sync {
    /// Runs before any client handler. Failure prevents dispatch.
    fn before<'a>(
        &'a self,
        name: &'a str,
        input: &'a Value,
        invocation: &'a ClaudeToolInvocation,
    ) -> ClaudeHookFuture<'a, Result<ClaudeToolDecision, String>>;

    /// Observe the exact result, including failures. A failure here is appended
    /// to the result; it never erases evidence of a completed tool effect.
    fn after<'a>(
        &'a self,
        _name: &'a str,
        _input: &'a Value,
        _invocation: &'a ClaudeToolInvocation,
        _reply: &'a ClaudeToolReply,
    ) -> ClaudeHookFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
}
