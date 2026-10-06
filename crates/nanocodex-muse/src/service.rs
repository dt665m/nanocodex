//! Muse-owned HTTP orchestration using Nanocodex's request and SSE machinery.
use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use nanocodex_oai_api::{
    OpenAiError,
    tower::{
        ResponsesAttempt, ResponsesAttemptKind, ResponsesOutput, ResponsesRetryPolicy,
        ResponsesServiceConfig, ResponsesServiceError, ResponsesServiceFactory,
        ResponsesServiceResponse,
    },
    transport::{ResponsesError, ResponsesTransport, http::ResponsesHttp},
};
use tower::{Service, retry::Retry};
use web_time::Instant;

/// HTTP-only service factory installed by the Muse recipe.
#[doc(hidden)]
#[derive(Clone)]
pub struct MuseServiceFactory;

impl ResponsesServiceFactory for MuseServiceFactory {
    type Service = Retry<ResponsesRetryPolicy, MuseService>;

    fn validate_config(&self, config: &ResponsesServiceConfig) -> Result<(), OpenAiError> {
        if config.responses_transport != ResponsesTransport::Https {
            return Err(OpenAiError::InvalidConfiguration {
                detail: "Muse requires HTTP Responses",
            });
        }
        Ok(())
    }

    fn make(&self, config: Arc<ResponsesServiceConfig>) -> Self::Service {
        #[cfg(not(target_family = "wasm"))]
        let http = {
            nanocodex_oai_api::transport::install_default_rustls_crypto_provider();
            ResponsesHttp::new(reqwest::Client::new())
        };
        #[cfg(target_family = "wasm")]
        let http = ResponsesHttp::new(config.host_transport.clone());
        Retry::new(
            ResponsesRetryPolicy::for_config(ResponsesRetryPolicy::DEFAULT_MAX_ATTEMPTS, &config),
            MuseService {
                config,
                http: http.with_headers(false, &[("x-api-version", "1.0.0")]),
                turn_state: Arc::new(tokio::sync::Mutex::new(None)),
            },
        )
    }
}

/// One session's Muse HTTP service, including its retained turn-state header.
#[doc(hidden)]
#[derive(Clone)]
pub struct MuseService {
    config: Arc<ResponsesServiceConfig>,
    http: ResponsesHttp,
    turn_state: Arc<tokio::sync::Mutex<Option<String>>>,
}

#[cfg(not(target_family = "wasm"))]
type ServiceFuture =
    Pin<Box<dyn Future<Output = Result<ResponsesServiceResponse, ResponsesServiceError>> + Send>>;
#[cfg(target_family = "wasm")]
type ServiceFuture =
    Pin<Box<dyn Future<Output = Result<ResponsesServiceResponse, ResponsesServiceError>>>>;

impl Service<ResponsesAttempt> for MuseService {
    type Response = ResponsesServiceResponse;
    type Error = ResponsesServiceError;
    type Future = ServiceFuture;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, request: ResponsesAttempt) -> Self::Future {
        let mut service = self.clone();
        Box::pin(async move {
            service
                .run(&request)
                .await
                .map_err(|error| error.with_request_input(&request))
        })
    }
}

impl MuseService {
    async fn run(
        &mut self,
        request: &ResponsesAttempt,
    ) -> Result<ResponsesServiceResponse, ResponsesServiceError> {
        if matches!(request.kind(), ResponsesAttemptKind::Warmup) {
            return Err(ResponsesServiceError::protocol(
                "Muse HTTP does not perform a warmup request",
            ));
        }
        let mut turn_state = self.turn_state.lock().await;
        let started_at = Instant::now();
        let encoded = crate::responses::encode(
            request.encode_generation(&self.config, turn_state.as_deref())?,
            matches!(request.kind(), ResponsesAttemptKind::Compaction),
        )?;
        request.record_http_request(&encoded)?;
        let auth =
            self.config
                .auth
                .snapshot()
                .await
                .map_err(|error| ResponsesError::Authorization {
                    detail: error.to_string(),
                })?;
        let profile = request.profile();
        let send = self
            .http
            .send(
                &self.config.api_base_url,
                &auth,
                profile.session_id(),
                profile.thread_id(),
                turn_state.as_deref(),
                &encoded,
            )
            .await;
        let (mut response, metadata) = match send {
            Err(ResponsesError::HttpRejected { status: 401, .. }) => {
                self.config
                    .auth
                    .recover_unauthorized(&auth)
                    .await
                    .map_err(|error| ResponsesError::Authorization {
                        detail: error.to_string(),
                    })?;
                let refreshed = self.config.auth.snapshot().await.map_err(|error| {
                    ResponsesError::Authorization {
                        detail: error.to_string(),
                    }
                })?;
                self.http
                    .send(
                        &self.config.api_base_url,
                        &refreshed,
                        profile.session_id(),
                        profile.thread_id(),
                        turn_state.as_deref(),
                        &encoded,
                    )
                    .await?
            }
            result => result?,
        };
        *turn_state = metadata.turn_state;
        let generated = response.receive_generation(request, started_at).await?;
        let output = crate::responses::decode(ResponsesOutput::Generation(generated), request)?;
        Ok(ResponsesServiceResponse::new(output)
            .with_attempt(request.attempt())
            .with_server_reasoning_included(metadata.reasoning_included))
    }
}
