//! Provider-owned wire translation around the shared Responses transport.
use crate::{
    tower::{ResponsesAttempt, ResponsesAttemptKind, ResponsesOutput, ResponsesServiceError},
    transport::{EncodedRequest, ResponsesError},
};

/// Optional provider wire adapter. The default transport remains unchanged.
pub trait ResponsesDialect: Send + Sync {
    /// Translates the encoded request to the provider's Responses format.
    /// # Errors
    /// Returns a protocol encoding failure.
    fn encode(
        &self,
        encoded: EncodedRequest,
        kind: ResponsesAttemptKind,
    ) -> Result<EncodedRequest, ResponsesError>;
    /// Selects the streamed response shape for this operation.
    fn receive_kind(&self, kind: ResponsesAttemptKind) -> ResponsesAttemptKind {
        kind
    }
    /// Translates provider output into the canonical typed result.
    /// # Errors
    /// Returns a provider protocol failure.
    fn decode(
        &self,
        output: ResponsesOutput,
        request: &ResponsesAttempt,
    ) -> Result<ResponsesOutput, ResponsesServiceError>;
    /// Whether HTTP requests use Responses Lite headers.
    fn responses_lite_headers(&self) -> bool {
        true
    }
    /// Additional public HTTP headers, without authorization material.
    fn http_headers(&self) -> &'static [(&'static str, &'static str)] {
        &[]
    }
    /// Whether a rejected API key should use the auth source's recovery hook.
    fn recover_api_key(&self) -> bool {
        false
    }
}
