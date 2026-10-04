//! Public native facade journeys. Only the HTTP model is a fixture: durable
//! builders, native descendants, tool transport and lifetime barriers are real.
use axum::{Json, Router, routing::post};
use nanocodex::{
    DurableAgentExt, Nanocodex, OpenAi, PromptRequest,
    durability::{DurableSession, SqliteStore},
    oai::transport::ResponsesTransport,
    tools::Tools,
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::Notify;

const DEADLINE: Duration = Duration::from_secs(20);

struct Fixture {
    base: String,
    requests: Arc<Mutex<Vec<Value>>>,
    child_started: Arc<Notify>,
    child_release: Arc<Notify>,
    parent_finished: Arc<Notify>,
    child_finished: Arc<Notify>,
    claude: bool,
    preserve_workspace: bool,
    server: tokio::task::JoinHandle<()>,
    workspace: PathBuf,
}

fn latest_prompt(body: &Value) -> String {
    let messages = body.get("input").unwrap_or(&body["messages"]);
    messages
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|item| item["role"] == "user" && !item["content"].to_string().contains("tool_result"))
        .map(|item| item["content"].to_string())
        .unwrap_or_default()
}

fn function(name: &str, arguments: Value, ordinal: usize) -> Value {
    json!([{"type":"function_call", "call_id":format!("native-call-{ordinal}"),
        "name":name, "arguments":arguments.to_string()}])
}

fn message(text: impl Into<String>) -> Value {
    json!([{"type":"message","role":"assistant","content":[{"type":"output_text","text":text.into()}]}])
}

impl Fixture {
    async fn start(label: &str) -> Self {
        Self::start_family(label, false, None).await
    }

    async fn start_family(label: &str, claude: bool, path: Option<PathBuf>) -> Self {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
        let child_started = Arc::new(Notify::new());
        let child_release = Arc::new(Notify::new());
        let parent_finished = Arc::new(Notify::new());
        let child_finished = Arc::new(Notify::new());
        let preserve_workspace = path.is_some();
        let workspace = path.unwrap_or_else(|| std::env::temp_dir().join(format!(
            "native-facade-{label}-{}",
            nanocodex::agent::session::SessionId::new()
        )));
        std::fs::create_dir_all(&workspace).unwrap();
        let trace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../../output/native-{label}-{}.json",
            std::process::id()
        ));
        std::fs::create_dir_all(trace.parent().unwrap()).unwrap();
        println!("HTTP_TRANSCRIPT {}", trace.display());
        let handler = post({
            let requests = requests.clone();
            let started = child_started.clone();
            let release = child_release.clone();
            let parent_finished = parent_finished.clone();
            let child_finished = child_finished.clone();
            move |Json(body): Json<Value>| {
                let requests = requests.clone();
                let started = started.clone();
                let release = release.clone();
                let parent_finished = parent_finished.clone();
                let child_finished = child_finished.clone();
                let trace = trace.clone();
                async move {
                    let ordinal = {
                        let mut requests = requests.lock().unwrap();
                        requests.push(body.clone());
                        std::fs::write(&trace, serde_json::to_vec_pretty(&*requests).unwrap()).unwrap();
                        requests.len()
                    };
                    let prompt = latest_prompt(&body);
                    let messages = body.get("input").unwrap_or(&body["messages"]).as_array().unwrap();
                    let last = messages.last().unwrap();
                    let continuation = last["type"] == "function_call_output" || last["content"][0]["type"] == "tool_result";
                    let output = if continuation {
                        if prompt.contains("spawn-") { parent_finished.notify_one(); }
                        if prompt.contains("gated-child") { child_finished.notify_one(); }
                        let reply = last.get("output").unwrap_or(&last["content"][0]["content"]);
                        let reply = reply.as_str().map(str::to_owned).unwrap_or_else(|| {
                            reply.as_array().map(|blocks| blocks.iter().filter_map(|block| block["text"].as_str()).collect::<Vec<_>>().join("\n")).unwrap_or_else(|| reply.to_string())
                        });
                        message(reply)
                    } else if prompt.contains("gated-child") {
                        started.notify_one();
                        release.notified().await;
                        function("submit_result", json!({"output":"native-child-result"}), ordinal)
                    } else if prompt.contains("spawn-foreground") || prompt.contains("spawn-background") {
                        let background = prompt.contains("spawn-background");
                        function("spawn_agent", json!({
                            "role":"native specialist", "task":"gated-child",
                            "harness":null,"model":null,"thinking":null,
                            "lifetime":if background {"background"} else {"foreground"},
                            "output_contract":{"kind":"string"}
                        }), ordinal)
                    } else if prompt.contains("directory") {
                        function("list_agents", json!({"include_completed":true,"include_self":false}), ordinal)
                    } else if prompt.contains("wait-background") {
                        function("wait_agent", json!({"agent_ids":[2],"timeout_ms":10000}), ordinal)
                    } else {
                        message("durable native answer")
                    };
                    let stream = if claude {
                        let item = &output[0];
                        let (content, stop) = if item["type"] == "function_call" {
                            (json!({"type":"tool_use","id":item["call_id"],"name":item["name"],"input":serde_json::from_str::<Value>(item["arguments"].as_str().unwrap()).unwrap()}), "tool_use")
                        } else {
                            (json!({"type":"text","text":item["content"][0]["text"]}), "end_turn")
                        };
                        [
                            json!({"type":"message_start","message":{"id":format!("message-{ordinal}"),"role":"assistant","model":"claude-sonnet-5-5","content":[],"usage":{"input_tokens":5,"output_tokens":0}}}),
                            json!({"type":"content_block_start","index":0,"content_block":content}),
                            json!({"type":"content_block_stop","index":0}),
                            json!({"type":"message_delta","delta":{"stop_reason":stop},"usage":{"output_tokens":3}}),
                            json!({"type":"message_stop"}),
                        ].into_iter().map(|frame| format!("data: {frame}\n\n")).collect()
                    } else {
                        let frame = json!({"type":"response.completed", "response":{"id":format!("response-{ordinal}"),"status":"completed","output":output}});
                        format!("data: {frame}\n\ndata: [DONE]\n\n")
                    };
                    ([ ("content-type", "text/event-stream") ], stream)
                }
            }
        });
        let app = Router::new().route("/responses", handler.clone()).route("/v1/messages", handler);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            base: format!("http://{address}"),
            requests,
            child_started,
            child_release,
            parent_finished,
            child_finished,
            claude,
            preserve_workspace,
            server,
            workspace,
        }
    }

    fn openai(&self) -> OpenAi {
        OpenAi::builder("synthetic-key")
            .transport(ResponsesTransport::Https)
            .websocket_warmup(false)
            .store(false)
            .api_base_url(&self.base)
            .build()
            .unwrap()
    }

    async fn parent(&self) -> (Nanocodex, nanocodex::AgentEvents) {
        let state = DurableSession::open(
            SqliteStore::open(self.workspace.join("children.sqlite")).unwrap(),
            "configured-native-root",
        )
        .await
        .unwrap();
        #[cfg(feature = "claude")]
        if self.claude {
            let client = nanocodex::claude::ClaudeClient::new(reqwest::Client::new(), format!("{}/v1/messages", self.base), "synthetic-key");
            return Nanocodex::builder(nanocodex::Claude::new(client, "claude-sonnet-5-5"))
                .durability(state).await.unwrap().build().unwrap();
        }
        Nanocodex::builder(self.openai())
            .workspace(&self.workspace)
            .tools(Tools::builder().without_defaults().build().unwrap())
            .durability(state)
            .await
            .unwrap()
            .build()
            .unwrap()
    }

    fn calls(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
        if !self.preserve_workspace {
            let _ = std::fs::remove_dir_all(&self.workspace);
        }
    }
}

async fn answer(agent: &Nanocodex, input: &str, id: &str) -> String {
    tokio::time::timeout(DEADLINE, async {
        agent
            .prompt(PromptRequest::new(input).request_id(id))
            .await
            .unwrap()
            .result()
            .await
            .unwrap()
            .final_message()
            .to_owned()
    })
    .await
    .expect("native facade prompt deadline")
}

#[tokio::test]
async fn configured_native_parent_own_spawn_and_descendant_replay_requests() {
    replay_journey(false).await;
}

#[cfg(feature = "claude")]
#[tokio::test]
async fn configured_claude_parent_owns_same_family_children_and_descendant_replay() {
    replay_journey(true).await;
}

async fn replay_journey(claude: bool) {
    let fixture = Fixture::start_family("facade-replay", claude, None).await;
    let (parent, parent_events) = fixture.parent().await;
    let root_session = parent.session_id().to_owned();
    let (child, child_events) = parent.spawn().await.unwrap();
    let (descendant, descendant_events) = child.spawn().await.unwrap();
    assert_ne!(parent.session_id(), child.session_id());
    assert_ne!(child.session_id(), descendant.session_id());
    for (agent, marker) in [
        (&parent, "root-own-request"),
        (&child, "child-own-request"),
        (&descendant, "descendant-own-request"),
    ] {
        assert_eq!(
            answer(agent, marker, "shared-request-id").await,
            "durable native answer"
        );
        let before = fixture.calls();
        assert_eq!(
            answer(agent, marker, "shared-request-id").await,
            "durable native answer"
        );
        assert_eq!(
            fixture.calls(),
            before,
            "{marker} replay must not call HTTP model"
        );
    }
    assert_eq!(
        fixture.calls(),
        3,
        "each session has an independent durable request identity"
    );
    descendant.shutdown().await.unwrap();
    child.shutdown().await.unwrap();
    parent.shutdown().await.unwrap();
    drop((
        parent,
        parent_events,
        child,
        child_events,
        descendant,
        descendant_events,
    ));
    // Fresh SQLite connection and facade builder; no live durable/session/registry
    // object is reused. The non-UUID host key must restore the native identity.
    let (reopened, reopened_events) = fixture.parent().await;
    assert_eq!(reopened.session_id(), root_session);
    let before = fixture.calls();
    assert_eq!(
        answer(&reopened, "root-own-request", "shared-request-id").await,
        "durable native answer"
    );
    assert_eq!(
        fixture.calls(),
        before,
        "cold root replay must not call HTTP model"
    );
    println!(
        "NATIVE_REPLAY parent.spawn + child.spawn: 3 independent HTTP requests, zero replay requests; SQLite cold root identity/receipt retained"
    );
    reopened.shutdown().await.unwrap();
    drop((reopened, reopened_events));
}

#[tokio::test]
async fn configured_native_registry_tools_hold_foreground_and_allow_background_then_reopen() {
    lifetime_journey(false).await;
}

#[cfg(feature = "claude")]
#[tokio::test]
async fn configured_claude_registry_holds_foreground_and_reopens_background_outputs() {
    lifetime_journey(true).await;
}

async fn lifetime_journey(claude: bool) {
    let fixture = Fixture::start_family("facade-lifetime", claude, None).await;
    let (parent, parent_events) = fixture.parent().await;
    let root_session = parent.session_id().to_owned();
    let foreground = parent
        .prompt(PromptRequest::new("spawn-foreground").request_id("foreground"))
        .await
        .unwrap();
    tokio::time::timeout(DEADLINE, fixture.child_started.notified())
        .await
        .unwrap();
    tokio::time::timeout(DEADLINE, fixture.parent_finished.notified())
        .await
        .unwrap();
    let mut foreground_result = Box::pin(foreground.result());
    assert!(
        tokio::time::timeout(Duration::from_millis(150), &mut foreground_result)
            .await
            .is_err(),
        "parent model finished but foreground native child must hold completion"
    );
    fixture.child_release.notify_one();
    let receipt = tokio::time::timeout(DEADLINE, foreground_result)
        .await
        .unwrap()
        .unwrap();
    assert!(receipt.final_message().contains("agent_id"));
    let directory: Value =
        serde_json::from_str(&answer(&parent, "directory", "directory-before-background").await)
            .unwrap();
    assert_eq!(directory["agents"].as_array().unwrap().len(), 1);
    assert_eq!(directory["agents"][0]["status"]["state"], "completed");
    assert_eq!(
        directory["agents"][0]["status"]["output"],
        "native-child-result"
    );
    assert_eq!(directory["agents"][0]["lifetime"], "foreground");

    let background = parent
        .prompt(PromptRequest::new("spawn-background").request_id("background"))
        .await
        .unwrap();
    tokio::time::timeout(DEADLINE, fixture.child_started.notified())
        .await
        .unwrap();
    let receipt = tokio::time::timeout(DEADLINE, background.result())
        .await
        .expect("background child must allow parent completion")
        .unwrap();
    assert!(receipt.final_message().contains("agent_id"));
    let directory: Value =
        serde_json::from_str(&answer(&parent, "directory", "directory-running-background").await)
            .unwrap();
    let background = directory["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|agent| agent["agent_id"] == 2)
        .unwrap();
    assert_eq!(background["lifetime"], "background");
    assert!(matches!(
        background["status"]["state"].as_str(),
        Some("running" | "pending")
    ));
    fixture.child_release.notify_one();
    let completed: Value =
        serde_json::from_str(&answer(&parent, "wait-background", "wait-background").await).unwrap();
    assert_eq!(completed["agents"][0]["status"]["state"], "completed");
    assert_eq!(
        completed["agents"][0]["status"]["output"],
        "native-child-result"
    );
    let calls_before = fixture.calls();
    let replay = answer(&parent, "spawn-background", "background").await;
    assert!(replay.contains("agent_id"));
    assert_eq!(
        fixture.calls(),
        calls_before,
        "spawn receipt request replay must not rerun HTTP or create a child"
    );
    parent.shutdown().await.unwrap();
    drop((parent, parent_events));
    let (reopened, reopened_events) = fixture.parent().await;
    assert_eq!(reopened.session_id(), root_session);
    let calls_before = fixture.calls();
    assert_eq!(
        answer(&reopened, "spawn-background", "background").await,
        replay
    );
    assert_eq!(
        fixture.calls(),
        calls_before,
        "cold parent spawn receipt replay is provider-free"
    );
    let restored: Value =
        serde_json::from_str(&answer(&reopened, "directory", "directory-after-reopen").await)
            .unwrap();
    assert_eq!(restored, directory_completed(&directory));
    println!(
        "NATIVE_LIFETIME real spawn_agent/list_agents/wait_agent/submit_result transport: foreground held after parent model terminal; background running after parent terminal; both completed outputs and topology retained on fresh SQLite/facade reopen"
    );
    reopened.shutdown().await.unwrap();
    drop((reopened, reopened_events));
}

fn directory_completed(directory: &Value) -> Value {
    let mut directory = directory.clone();
    for agent in directory["agents"].as_array_mut().unwrap() {
        agent["status"] = json!({"state":"completed", "output":"native-child-result"});
    }
    directory
}
