//! Public Agent/HTTP/QuickJS journey against SQLite across separate processes.
#![cfg(feature = "sqlite")]
use std::{collections::VecDeque, sync::{Arc, Mutex}};
use axum::{Json, Router, extract::State, response::IntoResponse, routing::post};
use nanocodex_agent::{Nanocodex, OpenAi, PromptRequest, Tools};
use nanocodex_oai_api::transport::ResponsesTransport;
use nanocodex_durability::{DurableAgentExt, DurableSession, SqliteStore};
use nanocodex_oai_tools::{Tool, ToolContext, ToolDefinition, ToolInput, ToolOutput, ToolResult};
use serde_json::{Value, json};

#[test]
fn native_cells_survive_process_restart_and_historical_fork() {
    let directory = tempfile::tempdir().unwrap();
    for stage in ["seed", "restart-and-fork", "verify"] {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "native_code_mode::native_process", "--ignored", "--nocapture"])
            .env("NANOCODEX_CODE_JOURNEY_DIR", directory.path())
            .env("NANOCODEX_CODE_JOURNEY_STAGE", stage).output().unwrap();
        println!("stage={stage}\n{}", String::from_utf8_lossy(&result.stdout));
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    }
}
struct Effect { directory: std::path::PathBuf }
#[async_trait::async_trait]
impl Tool for Effect {
    fn definition(&self) -> ToolDefinition { ToolDefinition::function("effect", "Records one external dispatch.", json!({"type":"object","properties":{}})) }
    async fn execute(&self, _: ToolInput, _: ToolContext<'_>) -> ToolResult {
        let path = self.directory.join("dispatches");
        let count = std::fs::read_to_string(&path).unwrap_or_default().parse::<u64>().unwrap_or(0) + 1;
        std::fs::write(path, count.to_string()).unwrap();
        Ok(ToolOutput::text(format!("EFFECT_{count}")))
    }
}
#[derive(Clone)]
struct Provider { responses: Arc<Mutex<VecDeque<(Value, Option<String>)>>> }
async fn responses(State(provider): State<Provider>, Json(request): Json<Value>) -> impl IntoResponse {
    let (output, expected) = provider.responses.lock().unwrap().pop_front().expect("unexpected model request");
    if let Some(expected) = expected {
        let last = request["input"].as_array().unwrap().iter().rev().find(|item| item["type"] == "custom_tool_call_output").unwrap();
        assert!(last["output"].to_string().contains(&expected), "expected {expected} in latest user-visible Code Mode result: {last}");
    }
    println!("HTTP_JOURNEY {}", json!({"request":request,"response":output}));
    let event = json!({"type":"response.completed","response":{"id":uuid::Uuid::new_v4().to_string(),"status":"completed","output":[output],"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}});
    ([("content-type", "text/event-stream")], format!("data: {event}\n\ndata: [DONE]\n\n"))
}
fn plan(cells: &[(&str, &str)]) -> Provider {
    let mut responses = VecDeque::new();
    for (source, expected) in cells {
        // Provider IDs repeat across prompts; durable scope includes the model ordinal.
        responses.push_back((json!({"type":"custom_tool_call","call_id":"call0","name":"exec","input":source}), None));
        responses.push_back((json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"CELL_VERIFIED"}]}), Some((*expected).into())));
    }
    Provider { responses: Arc::new(Mutex::new(responses)) }
}
async fn build_agent(state: DurableSession, directory: &std::path::Path, cells: &[(&str, &str)]) -> (Nanocodex, nanocodex_agent::events::AgentEvents, tokio::task::JoinHandle<()>, Provider) {
    let provider = plan(cells);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new().route("/responses", post(responses)).with_state(provider.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap(); });
    let openai = OpenAi::builder("synthetic-test-key").transport(ResponsesTransport::Https).store(false).api_base_url(endpoint).build().unwrap();
    let tools = Tools::builder().without_defaults().tool(Effect { directory: directory.into() }).build().unwrap();
    let (agent, events) = Nanocodex::builder(openai).workspace(directory).tools(tools).durability(state).await.unwrap().build().unwrap();
    (agent, events, server, provider)
}
async fn prompt(agent: &Nanocodex, id: &str) {
    let result = agent.prompt(PromptRequest::new(id).request_id(id)).await.unwrap().result().await.unwrap();
    assert_eq!(result.final_message(), "CELL_VERIFIED");
}
async fn close(agent: Nanocodex, events: nanocodex_agent::events::AgentEvents, server: tokio::task::JoinHandle<()>, provider: Provider) {
    agent.shutdown().await.unwrap(); drop((agent, events)); server.abort();
    assert!(provider.responses.lock().unwrap().is_empty());
}
#[tokio::test]
#[ignore = "subprocess fixture driven by native public journey"]
async fn native_process() {
    let directory = std::path::PathBuf::from(std::env::var("NANOCODEX_CODE_JOURNEY_DIR").unwrap());
    let stage = std::env::var("NANOCODEX_CODE_JOURNEY_STAGE").unwrap();
    let database = directory.join("state.sqlite");
    let source = DurableSession::open(SqliteStore::open(&database).unwrap(), "source").await.unwrap();
    if stage == "seed" {
        let (agent, events, server, provider) = build_agent(source.clone(), &directory, &[
            ("store(\"memo\", 1); text(await tools.effect({})); text(load(\"memo\"));", "EFFECT_1"),
            ("store(\"memo\", 2); text(load(\"memo\"));", "2"),
            ("store(\"memo\", 3); throw new Error(\"SCRIPT_FAILURE\");", "SCRIPT_FAILURE"),
        ]).await;
        prompt(&agent, "seed").await;
        prompt(&agent, "newer").await;
        prompt(&agent, "failed").await;
        close(agent, events, server, provider).await;
        assert_eq!(source.document("nanocodex.code-mode.store").await.unwrap().unwrap().value, json!({"memo":2}));
    } else if stage == "restart-and-fork" {
        let (snapshot, documents) = source.agent_document_fork("seed").await.unwrap();
        let fork = DurableSession::open(SqliteStore::open(&database).unwrap(), "fork").await.unwrap();
        fork.initialize_agent_document_fork(documents, &snapshot).await.unwrap();
        let (agent, events, server, provider) = build_agent(source.clone(), &directory, &[("text({cold:load(\"memo\")});", "cold")]).await;
        // Exact terminal turn replay performs no model or external calls.
        prompt(&agent, "seed").await;
        prompt(&agent, "cold").await;
        close(agent, events, server, provider).await;
        let (agent, events, server, provider) = build_agent(fork.clone(), &directory, &[("if(load(\"memo\") !== 1) throw new Error(\"BAD_FORK\"); store(\"memo\", 9); text(\"FORK_VERIFIED\");", "FORK_VERIFIED")]).await;
        prompt(&agent, "fork-write").await;
        close(agent, events, server, provider).await;
        assert_eq!(source.document("nanocodex.code-mode.store").await.unwrap().unwrap().value, json!({"memo":2}));
        assert_eq!(fork.document("nanocodex.code-mode.store").await.unwrap().unwrap().value, json!({"memo":9}));
    } else {
        let (agent, events, server, provider) = build_agent(source.clone(), &directory, &[("if(load(\"memo\") !== 2) throw new Error(\"BAD_ROOT\"); text(\"ROOT_VERIFIED\");", "ROOT_VERIFIED")]).await;
        prompt(&agent, "root-check").await;
        close(agent, events, server, provider).await;
        let fork = DurableSession::open(SqliteStore::open(&database).unwrap(), "fork").await.unwrap();
        let (agent, events, server, provider) = build_agent(fork.clone(), &directory, &[("if(load(\"memo\") !== 9) throw new Error(\"BAD_FORK\"); text(\"FORK_COLD_VERIFIED\");", "FORK_COLD_VERIFIED")]).await;
        prompt(&agent, "fork-check").await;
        close(agent, events, server, provider).await;
    }
    assert_eq!(std::fs::read_to_string(directory.join("dispatches")).unwrap(), "1");
    println!("NATIVE_CODE_DURABLE_TRACE {}", json!({"stage":stage,"store":source.document("nanocodex.code-mode.store").await.unwrap(),"external_dispatches":1}));
}
