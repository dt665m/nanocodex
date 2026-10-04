//! Portable adaptation of Grok Build's Responses sampling and conversation core.
//!
//! Host applications supply credentials and tools. The shared Nanocodex lifecycle
//! wraps the xAI-native transcript; no Claude Messages conversion is performed.
//! See `UPSTREAM.md` for the pinned source and deliberate adaptation boundaries.
use futures_util::{FutureExt, StreamExt};
use nanocodex_agent::{
    AgentEvents, AgentSessionContext, CostStatus, HarnessFamily, HarnessModel, Model, Nanocodex,
    NanocodexError, ReportedTurnUsage, Result, SpawnOptions, Thinking, TurnResult, TurnUsage,
    backend::{
        BackendFuture, BackendPrompt, BackendPromptRoute, BackendRuntime, BackendTurn,
        BackendTurnKey, BuilderBackend, LifecycleBackend,
    },
    events::{AgentEvent, AgentEventKind, AgentEventPublisher},
    input::{Prompt, PromptInput, PromptMessageRole},
};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    future::Future,
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Mutex as AsyncMutex, Notify, oneshot};
mod conversation;
mod stream;
use web_time::Instant;

#[derive(Default)]
struct RunStats {
    model_calls: u32,
    tool_calls: u32,
    model_ns: u64,
    tool_ns: u64,
    usage: Option<TurnUsage>,
}
fn elapsed_ns(start: Instant) -> u64 {
    start.elapsed().as_nanos().min(u64::MAX as u128) as u64
}

/// Official public source revision from which this implementation is adapted.
pub const UPSTREAM_REVISION: &str = "2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8";
/// Monorepo revision recorded by upstream in SOURCE_REV.
pub const UPSTREAM_SOURCE_REVISION: &str = "559751fdcec02d413e4c57c8832ab275e4f44980";

/// Explicit xAI transport. Debug intentionally does not expose the credential.
#[derive(Clone)]
pub struct XaiClient {
    http: reqwest::Client,
    endpoint: String,
    key: Arc<str>,
}
impl XaiClient {
    /// Creates a client for a complete Responses URL (normally
    /// `https://api.x.ai/v1/responses`). The caller owns credential acquisition.
    pub fn new(
        http: reqwest::Client,
        endpoint: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        Self {
            http,
            endpoint: endpoint.into(),
            key: Arc::from(api_key.into()),
        }
    }
}
#[cfg(not(target_family = "wasm"))]
type ToolFuture = Pin<Box<dyn Future<Output = std::result::Result<String, String>> + Send>>;
#[cfg(target_family = "wasm")]
type ToolFuture = Pin<Box<dyn Future<Output = std::result::Result<String, String>>>>;
type Handler = Arc<dyn Fn(Value) -> ToolFuture + Send + Sync>;

/// A host-owned xAI function definition. The model never grants tool authority.
#[derive(Clone, Debug)]
pub struct ToolDefinition {
    /// Unique function name.
    pub name: String,
    /// Model-visible purpose.
    pub description: String,
    /// JSON object schema for arguments.
    pub parameters: Value,
}

/// Concrete xAI harness recipe and builder.
#[derive(Clone)]
pub struct Xai {
    client: XaiClient,
    model: String,
    thinking: Thinking,
    system: String,
    tools: HashMap<String, (ToolDefinition, Handler)>,
    hosted: Vec<Value>,
    max_steps: usize,
    timeout: Duration,
}
impl BuilderBackend for Xai {
    type Builder = Self;
    fn into_builder(self) -> Self {
        self
    }
}
impl Xai {
    /// Starts a native xAI recipe using an explicit provider model identifier.
    pub fn new(client: XaiClient, model: impl Into<String>) -> Self {
        Self {
            client,
            model: model.into(),
            thinking: Thinking::High,
            system: String::new(),
            tools: HashMap::new(),
            hosted: Vec::new(),
            max_steps: 32,
            timeout: Duration::from_secs(300),
        }
    }
    /// Sets the system instruction prepended to the native conversation.
    pub fn system(mut self, system: impl Into<String>) -> Self {
        self.system = system.into();
        self
    }
    /// Sets reasoning effort from the pinned upstream model catalog.
    pub const fn thinking(mut self, thinking: Thinking) -> Self {
        self.thinking = thinking;
        self
    }
    /// Bounds the number of model calls per submitted turn.
    pub const fn max_steps(mut self, steps: usize) -> Self {
        self.max_steps = steps;
        self
    }
    /// Bounds a complete HTTP sampling call, including its event stream.
    pub const fn request_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
    /// Registers an application-authorized native function.
    #[cfg(not(target_family = "wasm"))]
    pub fn tool<F, Fut>(mut self, definition: ToolDefinition, callback: F) -> Self
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = std::result::Result<String, String>> + Send + 'static,
    {
        self.tools.insert(
            definition.name.clone(),
            (definition, Arc::new(move |v| Box::pin(callback(v)))),
        );
        self
    }
    /// Registers an isolate-local application-authorized native function.
    #[cfg(target_family = "wasm")]
    pub fn tool<F, Fut>(mut self, definition: ToolDefinition, callback: F) -> Self
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = std::result::Result<String, String>> + 'static,
    {
        self.tools.insert(
            definition.name.clone(),
            (definition, Arc::new(move |v| Box::pin(callback(v)))),
        );
        self
    }
    /// Enables xAI-hosted web search. Its calls stay server-side and are replayed
    /// as native context; they never execute an application callback.
    pub fn web_search(mut self) -> Self {
        self.hosted.push(json!({"type":"web_search"}));
        self
    }
    /// Builds an in-memory lifecycle. Credentials are used only when prompting.
    pub fn build(self) -> Result<(Nanocodex, AgentEvents)> {
        validate_effort(&self.model, self.thinking)?;
        if self.model.trim().is_empty() || self.max_steps == 0 {
            return Err(invalid("xAI requires a model and max_steps > 0"));
        }
        if !self.hosted.is_empty() && self.tools.contains_key("web_search") {
            return Err(invalid(
                "host function web_search conflicts with the xAI-hosted tool",
            ));
        }
        for (definition, _) in self.tools.values() {
            if definition.name.trim().is_empty() || !definition.parameters.is_object() {
                return Err(invalid(
                    "xAI functions require a name and an object JSON schema",
                ));
            }
        }
        let url = reqwest::Url::parse(&self.client.endpoint).map_err(error)?;
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(invalid(
                "xAI endpoint must be HTTP(S) without URL credentials",
            ));
        }
        let session = uuid::Uuid::new_v4().to_string();
        let (runtime, events) = BackendRuntime::new(session.clone());
        let history = if self.system.is_empty() {
            Vec::new()
        } else {
            vec![json!({"type":"message","role":"system","content":self.system})]
        };
        let driver = Driver {
            state: Arc::new(State {
                config: Mutex::new(self),
                history: AsyncMutex::new(history),
                active: Mutex::new(None),
                stopped: AtomicBool::new(false),
                seq: AtomicU64::new(0),
                session,
            }),
        };
        Ok((runtime.bind(driver), events))
    }
}
fn invalid(message: impl Into<String>) -> NanocodexError {
    NanocodexError::InvalidRequest(message.into())
}
fn error(error: impl std::fmt::Display) -> NanocodexError {
    invalid(format!("xAI: {error}"))
}
fn effort(thinking: Thinking) -> Result<&'static str> {
    match thinking {
        Thinking::Low => Ok("low"),
        Thinking::Medium => Ok("medium"),
        Thinking::High => Ok("high"),
        Thinking::Xhigh => Ok("xhigh"),
        _ => Err(invalid(
            "xAI supports low, medium, high and xhigh reasoning effort",
        )),
    }
}
fn unsupported<T: Send + 'static>(operation: &'static str) -> BackendFuture<Result<T>> {
    Box::pin(async move { Err(invalid(format!("xAI harness does not support {operation}"))) })
}
#[derive(Default)]
struct Cancellation {
    cancelled: AtomicBool,
    notify: Notify,
}
impl Cancellation {
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.notify.notify_one();
    }
    fn check(&self) -> Result<()> {
        if self.cancelled.load(Ordering::SeqCst) {
            Err(NanocodexError::TurnCancelled)
        } else {
            Ok(())
        }
    }
    async fn wait(&self) {
        loop {
            let wait = self.notify.notified();
            if self.cancelled.load(Ordering::SeqCst) {
                return;
            }
            wait.await;
        }
    }
}
struct Active {
    key: BackendTurnKey,
    cancel: Arc<Cancellation>,
    done: Arc<Notify>,
}
struct State {
    config: Mutex<Xai>,
    history: AsyncMutex<Vec<Value>>,
    active: Mutex<Option<Active>>,
    stopped: AtomicBool,
    seq: AtomicU64,
    session: String,
}
#[derive(Clone)]
struct Driver {
    state: Arc<State>,
}
impl State {
    fn emit(&self, events: &AgentEventPublisher, kind: AgentEventKind, payload: Value) {
        let _ = events.publish(AgentEvent {
            protocol_version: 1,
            request_id: Arc::from(events.request_id()),
            seq: self.seq.fetch_add(1, Ordering::SeqCst),
            kind,
            payload: Arc::from(
                serde_json::value::to_raw_value(&payload).expect("JSON value serializes"),
            ),
        });
    }
    async fn sample(
        &self,
        config: &Xai,
        history: &[Value],
        events: &AgentEventPublisher,
        index: usize,
        cancel: &Cancellation,
    ) -> Result<Value> {
        let mut tools:Vec<Value>=config.tools.values().map(|(definition,_)|json!({"type":"function","name":definition.name,"description":definition.description,"parameters":definition.parameters})).collect();
        tools.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        tools.extend(config.hosted.clone());
        let mut input = history.to_vec();
        conversation::patch_reasoning_text_types(&mut input);
        let body = json!({"model":config.model,"input":input,"tools":tools,"stream":true,"store":false,
            "reasoning":{"effort":effort(config.thinking)?,"summary":"concise"},"prompt_cache_key":self.session});
        let request = async {
            let response = config
                .client
                .http
                .post(&config.client.endpoint)
                .bearer_auth(config.client.key.as_ref())
                .header("accept", "text/event-stream")
                .json(&body)
                .send()
                .await
                .map_err(|e| error(e.without_url()))?;
            if !response.status().is_success() {
                return Err(invalid(format!("xAI Responses HTTP {}", response.status())));
            }
            let mut bytes = response.bytes_stream();
            let mut decoder = stream::Decoder::default();
            while let Some(chunk) = bytes.next().await {
                for event in decoder
                    .push(&chunk.map_err(|e| error(e.without_url()))?)
                    .map_err(invalid)?
                {
                    self.emit(events,AgentEventKind::ApiEvent,json!({"provider":"xai","direction":"received","transport":"responses_sse","phase":"generation","model_call_index":index,"event":event}));
                    match event["type"].as_str() {
                        Some("response.output_text.delta")=>self.emit(events,AgentEventKind::AssistantDelta,json!({"model_call_index":index,"item_id":event["item_id"],"phase":null,"text":event["delta"]})),
                        Some("response.reasoning_summary_text.delta"|"response.reasoning_text.delta")=>self.emit(events,AgentEventKind::ReasoningSummaryDelta,json!({"model_call_index":index,"text":event["delta"]})),
                        _=>{},
                    }
                    if let Some(response) = stream::terminal(&event).map_err(invalid)? {
                        return Ok(response);
                    }
                }
            }
            Err(invalid(
                "xAI Responses stream ended without a completed response; no tools were dispatched",
            ))
        };
        tokio::select! {
            biased;
            _=cancel.wait()=>Err(NanocodexError::TurnCancelled),
            _=deadline(config.timeout)=>Err(invalid("xAI sampling timed out; request was not retried")),
            outcome=request=>outcome,
        }
    }
    async fn run(
        &self,
        config: Xai,
        prompt: Prompt,
        events: &AgentEventPublisher,
        cancel: &Cancellation,
        stats: &mut RunStats,
    ) -> Result<TurnResult> {
        let mut history = self.history.lock().await;
        let PromptInput::Text(text) = &prompt.instruction else {
            return Err(invalid("xAI currently accepts text prompts"));
        };
        let mut candidate = history.clone();
        for message in prompt.transcript() {
            candidate.push(json!({"type":"message","role":match message.role(){PromptMessageRole::User=>"user",PromptMessageRole::Assistant=>"assistant"},"content":message.content()}));
        }
        candidate.push(json!({"type":"message","role":"user","content":text}));
        let mut usage = ReportedTurnUsage {
            input_tokens: 0,
            cached_input_tokens: 0,
            cache_write_input_tokens: 0,
            output_tokens: 0,
            reasoning_output_tokens: 0,
            total_tokens: 0,
            estimated_cost: None,
            cost_status: CostStatus::Other,
        };
        let mut reported = false;
        let mut seen: HashSet<String> = history
            .iter()
            .filter(|item| item["type"] == "function_call")
            .filter_map(|item| item["call_id"].as_str().map(str::to_owned))
            .collect();
        for index in 0..config.max_steps {
            cancel.check()?;
            stats.model_calls = stats.model_calls.saturating_add(1);
            let sampled = Instant::now();
            let response = self
                .sample(&config, &candidate, events, index, cancel)
                .await;
            stats.model_ns = stats.model_ns.saturating_add(elapsed_ns(sampled));
            let response = response?;
            let output = response["output"]
                .as_array()
                .ok_or_else(|| invalid("xAI response output is not an array"))?;
            let calls = conversation::function_calls(output).map_err(invalid)?;
            // Validate every call before any side effect or history mutation.
            for call in &calls {
                if !seen.insert(call.call_id.clone()) {
                    return Err(invalid(
                        "xAI repeated a function call_id from committed history or this response",
                    ));
                }
            }
            candidate.extend(conversation::replay_output(output));
            *history = candidate.clone();
            if let Some(tokens) = response["usage"].as_object() {
                reported = true;
                usage.input_tokens = usage.input_tokens.saturating_add(
                    tokens
                        .get("input_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                );
                usage.output_tokens = usage.output_tokens.saturating_add(
                    tokens
                        .get("output_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                );
                usage.total_tokens = usage.total_tokens.saturating_add(
                    tokens
                        .get("total_tokens")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                );
                usage.cached_input_tokens = usage.cached_input_tokens.saturating_add(
                    response["usage"]["input_tokens_details"]["cached_tokens"]
                        .as_u64()
                        .unwrap_or(0),
                );
                usage.reasoning_output_tokens = usage.reasoning_output_tokens.saturating_add(
                    response["usage"]["output_tokens_details"]["reasoning_tokens"]
                        .as_u64()
                        .unwrap_or(0),
                );
            }
            stats.usage = reported.then(|| TurnUsage::from_reported(usage.clone()));
            if calls.is_empty() {
                let text = conversation::assistant_text(output);
                self.emit(
                    events,
                    AgentEventKind::AssistantMessage,
                    json!({"model_call_index":index,"item_id":null,"phase":null,"text":text}),
                );
                return Ok(TurnResult::from_backend(
                    None,
                    text,
                    reported.then(|| TurnUsage::from_reported(usage)),
                ));
            }
            for call in calls {
                stats.tool_calls = stats.tool_calls.saturating_add(1);
                let started = Instant::now();
                let input: std::result::Result<Value, _> = serde_json::from_str(&call.arguments);
                self.emit(events,AgentEventKind::ToolCall,json!({"call_id":call.call_id,"tool":call.name,"arguments":input.as_ref().cloned().unwrap_or_else(|_|Value::String(call.arguments.clone())),"model_call_index":index}));
                let result = if cancel.check().is_err() {
                    Err("Tool skipped because turn was cancelled".into())
                } else {
                    match (config.tools.get(&call.name), input) {
                        (Some((_, handler)), Ok(input)) if input.is_object() => {
                            // Once started, a host effect must finish before cancellation can
                            // release the session, so it cannot silently replay on the next turn.
                            std::panic::AssertUnwindSafe(async { handler(input).await })
                                .catch_unwind()
                                .await
                                .unwrap_or_else(|_| Err("Host tool panicked".into()))
                        }
                        (None, _) => Err(format!("Unknown tool: {}", call.name)),
                        _ => Err("Function arguments must be a JSON object".into()),
                    }
                };
                let (output, status) = match result {
                    Ok(text) => (text, "completed"),
                    Err(err) => (format!("Tool error: {err}"), "failed"),
                };
                let duration_ns = elapsed_ns(started);
                stats.tool_ns = stats.tool_ns.saturating_add(duration_ns);
                self.emit(events,AgentEventKind::ToolResult,json!({"call_id":call.call_id,"tool":call.name,"status":status,"duration_ns":duration_ns,"started_after_ns":null,"result":output,"structured_result":null,"metadata":null}));
                candidate.push(
                    json!({"type":"function_call_output","call_id":call.call_id,"output":output}),
                );
                *history = candidate.clone();
            }
        }
        Err(invalid(
            "xAI turn reached max_steps; completed tools remain in history",
        ))
    }
}
impl Driver {
    async fn admit(self, request: BackendPrompt) -> Result<BackendTurn> {
        if request.request_id.is_some() {
            return Err(invalid(
                "xAI in-memory harness does not provide durable request deduplication",
            ));
        }
        if !matches!(request.prompt.instruction, PromptInput::Text(_)) {
            return Err(invalid("xAI currently accepts text prompts"));
        }
        let config;
        let cancel = Arc::new(Cancellation::default());
        let done = Arc::new(Notify::new());
        {
            let mut active = self.state.active.lock().unwrap();
            if self.state.stopped.load(Ordering::SeqCst) {
                return Err(NanocodexError::AgentStopped);
            }
            if active.is_some() {
                return Err(invalid("xAI session already has an active turn"));
            }
            if request.cancel_on_admission {
                cancel.cancel();
            }
            config = self.state.config.lock().unwrap().clone();
            *active = Some(Active {
                key: request.key,
                cancel: cancel.clone(),
                done: done.clone(),
            });
        }
        let (sender, receiver) = oneshot::channel();
        let task = async move {
            let effort = effort(config.thinking).unwrap_or("high");
            self.state.emit(&request.events,AgentEventKind::RunStarted,json!({"mode":"xai","model":config.model,"reasoning_mode":"effort","effort":effort,"transport":"responses_sse","orchestration":"grok_build","websocket_url":"","workspace":null,"instruction_bytes":request.prompt.text_bytes()}));
            let started = Instant::now();
            let mut stats = RunStats::default();
            let result = self
                .state
                .run(
                    config.clone(),
                    request.prompt,
                    &request.events,
                    &cancel,
                    &mut stats,
                )
                .await;
            let duration_ns = elapsed_ns(started);
            if let Err(err) = &result {
                self.state.emit(
                    &request.events,
                    AgentEventKind::RunError,
                    json!({"message":err.to_string()}),
                );
            }
            self.state.emit(&request.events,if result.is_ok(){AgentEventKind::RunCompleted}else{AgentEventKind::RunFailed},json!({"status":if result.is_ok(){"completed"}else if matches!(&result,Err(NanocodexError::TurnCancelled)){"cancelled"}else{"failed"},"model":config.model,"reasoning_mode":"effort","effort":effort,"transport":"responses_sse","orchestration":"grok_build","duration_ms":duration_ns/1_000_000,"duration_ns":duration_ns,"estimated_cost":null,"cost_usd":null,"cost_status":"other","model_calls":stats.model_calls,"steers":0,"compactions":0,"tool_calls":stats.tool_calls,"connection_attempts":stats.model_calls,"websocket_reconnects":0,"response_attempts":stats.model_calls,"response_retries":0,"connection_duration_ns":0,"retry_backoff_duration_ns":0,"model_duration_ns":stats.model_ns,"compaction_duration_ns":0,"warmup_duration_ns":0,"tool_work_duration_ns":stats.tool_ns,"tool_wall_duration_ns":stats.tool_ns,"usage":stats.usage.as_ref().map(|usage| serde_json::to_value(usage).unwrap()).unwrap_or_else(||json!({"input_tokens":0,"cached_input_tokens":0,"cache_write_input_tokens":0,"output_tokens":0,"reasoning_output_tokens":0,"total_tokens":0})),"warmup_usage":{"input_tokens":0,"cached_input_tokens":0,"cache_write_input_tokens":0,"output_tokens":0,"reasoning_output_tokens":0,"total_tokens":0}}));
            self.state.active.lock().unwrap().take();
            done.notify_waiters();
            let _ = sender.send(result);
        };
        #[cfg(not(target_family = "wasm"))]
        tokio::spawn(task);
        #[cfg(target_family = "wasm")]
        wasm_bindgen_futures::spawn_local(task);
        Ok(BackendTurn {
            request_id: None,
            result: Box::pin(async { receiver.await.map_err(|_| NanocodexError::TurnStopped)? }),
        })
    }
    fn configure(
        &self,
        model: Option<String>,
        thinking: Option<Thinking>,
    ) -> BackendFuture<Result<()>> {
        let state = self.state.clone();
        Box::pin(async move {
            let active = state.active.lock().unwrap();
            if active.is_some() {
                return Err(invalid("xAI session is busy"));
            }
            if state.stopped.load(Ordering::SeqCst) {
                return Err(NanocodexError::AgentStopped);
            }
            let mut config = state.config.lock().unwrap();
            let selected_model = model.as_deref().unwrap_or(&config.model);
            let selected_thinking = thinking.unwrap_or(config.thinking);
            validate_effort(selected_model, selected_thinking)?;
            config.thinking = selected_thinking;
            if let Some(model) = model {
                config.model = model;
            }
            Ok(())
        })
    }
}
impl LifecycleBackend for Driver {
    fn harness_family(&self) -> HarnessFamily {
        HarnessFamily::Xai
    }
    fn submit(&self, request: BackendPrompt) -> BackendFuture<Result<BackendTurn>> {
        Box::pin(self.clone().admit(request))
    }
    fn route(&self, request: BackendPrompt) -> BackendFuture<Result<BackendPromptRoute>> {
        let driver = self.clone();
        Box::pin(async move { driver.admit(request).await.map(BackendPromptRoute::Started) })
    }
    fn steer(&self, _: BackendTurnKey, _: Prompt) -> BackendFuture<Result<()>> {
        unsupported("live steering")
    }
    fn cancel(&self, key: BackendTurnKey) -> BackendFuture<Result<()>> {
        let state = self.state.clone();
        Box::pin(async move {
            let done = {
                let active = state.active.lock().unwrap();
                active.as_ref().filter(|a| a.key == key).map(|a| {
                    a.cancel.cancel();
                    a.done.clone()
                })
            };
            if let Some(done) = done {
                let notified = done.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                let still_active = state
                    .active
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_some_and(|a| a.key == key);
                if still_active {
                    notified.await;
                }
            }
            Ok(())
        })
    }
    fn set_model(&self, _: Model) -> BackendFuture<Result<()>> {
        unsupported("OpenAI model selectors")
    }
    fn set_harness_model(&self, model: HarnessModel) -> BackendFuture<Result<()>> {
        if model.family() != HarnessFamily::Xai {
            return unsupported("another harness family");
        }
        self.configure(Some(model.as_str().into()), None)
    }
    fn set_thinking(&self, thinking: Thinking) -> BackendFuture<Result<()>> {
        self.configure(None, Some(thinking))
    }
    fn set_fast_mode(&self, _: bool) -> BackendFuture<Result<()>> {
        unsupported("fast mode")
    }
    fn compact(&self) -> BackendFuture<Result<()>> {
        unsupported("automatic or manual compaction")
    }
    fn append_developer_message(&self, _: String) -> BackendFuture<Result<AgentSessionContext>> {
        unsupported("developer message injection")
    }
    fn context(&self) -> BackendFuture<Result<AgentSessionContext>> {
        unsupported("OpenAI transcript conversion; native xAI history is retained internally")
    }
    fn spawn(&self, options: SpawnOptions) -> BackendFuture<Result<(Nanocodex, AgentEvents)>> {
        let mut recipe = self.state.config.lock().unwrap().clone();
        let state = self.state.clone();
        Box::pin(async move {
            if state.stopped.load(Ordering::SeqCst) {
                return Err(NanocodexError::AgentStopped);
            }
            options.validate_harness()?;
            if options
                .selected_harness()
                .is_some_and(|f| f != HarnessFamily::Xai)
                || options
                    .selected_harness_model()
                    .is_some_and(|m| m.family() != HarnessFamily::Xai)
            {
                return Err(invalid("xAI can only spawn its own family"));
            }
            if let Some(model) = options.selected_harness_model() {
                recipe.model = model.as_str().into();
            }
            if let Some(thinking) = options.selected_thinking() {
                recipe.thinking = thinking;
            }
            recipe.build()
        })
    }
    fn fork(&self, _: Option<TurnResult>) -> BackendFuture<Result<(Nanocodex, AgentEvents)>> {
        unsupported("forking")
    }
    fn flush(&self) -> BackendFuture<Result<()>> {
        Box::pin(async { Ok(()) })
    }
    fn shutdown(&self) -> BackendFuture<Result<()>> {
        let state = self.state.clone();
        Box::pin(async move {
            state.stopped.store(true, Ordering::SeqCst);
            loop {
                let done = {
                    let active = state.active.lock().unwrap();
                    active.as_ref().map(|a| {
                        a.cancel.cancel();
                        a.done.clone()
                    })
                };
                let Some(done) = done else { return Ok(()) };
                let notified = done.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if state.active.lock().unwrap().is_none() {
                    return Ok(());
                }
                notified.await;
            }
        })
    }
}

fn validate_effort(model: &str, thinking: Thinking) -> Result<()> {
    effort(thinking)?;
    if model
        .parse::<nanocodex_agent::XaiModel>()
        .is_ok_and(|model| !model.supports_thinking(thinking))
    {
        return Err(invalid(
            "unsupported reasoning effort for selected xAI model",
        ));
    }
    Ok(())
}
#[cfg(not(target_family = "wasm"))]
async fn deadline(duration: Duration) {
    tokio::time::sleep(duration).await;
}
#[cfg(target_family = "wasm")]
async fn deadline(duration: Duration) {
    use wasm_bindgen::{JsCast, JsValue};
    // Use the JavaScript host clock; a browser/Worker has no Tokio timer driver.
    let promise = js_sys::Promise::new(&mut |resolve, reject| {
        let result = js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("setTimeout"))
            .and_then(|function| function.dyn_into::<js_sys::Function>())
            .and_then(|function| {
                function.call2(
                    &JsValue::UNDEFINED,
                    &resolve,
                    &JsValue::from_f64(duration.as_millis().min(i32::MAX as u128) as f64),
                )
            });
        if let Err(error) = result {
            let _ = reject.call1(&JsValue::UNDEFINED, &error);
        }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}
