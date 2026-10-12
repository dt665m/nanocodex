//! Public runtime journey. Only the external providers are local HTTP
//! fixtures; the runtime, recipes, tool bridges, subagent registry, routing,
//! resume and shutdown run normally.
use axum::{Json, Router, routing::post};
#[cfg(any(feature = "claude", feature = "durability"))]
use nanocodex::Thinking;
#[cfg(feature = "claude")]
use nanocodex::{
    ClaudeHarness, ClaudeModel, HarnessModel, SessionCheckpoint, claude::ClaudeClient,
};
use nanocodex::{
    CodexHarness, HarnessFamily, Model, Nanocodex, NanocodexError, OpenAi,
    agent::{AgentHandle, SpawnOptions},
    oai::transport::ResponsesTransport,
    tool,
    tools::Tools,
};
use nanocodex_subagents::{AgentId, AgentStatus, Registry, channel, install_tools};
#[cfg(feature = "claude")]
use nanocodex_subagents::{AgentTask, start_agent_with};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
#[cfg(feature = "claude")]
use tokio::sync::Semaphore;

const DEADLINE: Duration = Duration::from_secs(10);
const SUBMIT: &str = "const receipt = await tools.submit_result({output: 'native-result'}); if (!receipt.accepted) throw Error('result was not accepted'); text(receipt);";
type Handles = Arc<Mutex<HashMap<String, AgentHandle>>>;
type Transcript = Arc<Mutex<Vec<Value>>>;

#[tool(name = "runtime_echo", description = "Echoes the supplied text.")]
async fn echo(text: String) -> Result<String, &'static str> {
    Ok(format!("echo:{text}"))
}

fn echo_tools() -> Tools {
    Tools::builder()
        .without_defaults()
        .tool(echo)
        .build()
        .unwrap()
}

fn registry_tools(handles: &Handles, handle: AgentHandle, registry: Arc<Registry>) -> Tools {
    handles
        .lock()
        .unwrap()
        .insert(handle.session_id().into(), handle.clone());
    install_tools(
        Tools::builder().without_defaults().build().unwrap(),
        handle,
        registry,
    )
    .unwrap()
}

fn named(tools: &Value, name: &str) -> bool {
    tools
        .as_array()
        .is_some_and(|tools| tools.iter().any(|tool| tool["name"] == name))
}

/// Whether a provider request declares a tool, top-level or as Responses
/// additional tools.
fn declares(body: &Value, name: &str) -> bool {
    named(&body["tools"], name)
        || body["input"].as_array().is_some_and(|input| {
            input
                .iter()
                .any(|item| item["type"] == "additional_tools" && named(&item["tools"], name))
        })
}

fn responses(body: &Value, ordinal: usize) -> String {
    let input = body["input"].as_array().unwrap();
    let continuation = input.last().is_some_and(|item| {
        item["type"] == "custom_tool_call_output" || item["type"] == "function_call_output"
    });
    let last_user = input
        .iter()
        .rev()
        .find(|item| item["role"] == "user")
        .map(Value::to_string)
        .unwrap_or_default();
    let has_exec = declares(body, "exec");
    let output = if continuation {
        json!([{"type":"message","role":"assistant","content":[{"type":"output_text","text":"native done"}]}])
    } else if last_user.contains("[delegate-muse]") {
        let description = body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == "exec")
            .unwrap()["description"]
            .as_str()
            .unwrap();
        let declaration = description
            .split("spawn_agent(args:")
            .nth(1)
            .unwrap_or_default();
        let model = declaration
            .lines()
            .find(|line| line.trim_start().starts_with("model:"))
            .and_then(|line| {
                line.split('"')
                    .skip(1)
                    .step_by(2)
                    .filter(|model| last_user.contains(model))
                    .max_by_key(|model| model.len())
            });
        let family = declaration
            .lines()
            .find(|line| line.trim_start().starts_with("harness:"))
            .is_some_and(|line| {
                line.split('"')
                    .skip(1)
                    .step_by(2)
                    .any(|family| family == "muse")
            });
        match (model, family) {
            (Some(model), true) => {
                let args = json!({"role":"synthetic Muse specialist", "task":"[submit] Complete the assignment.",
                    "harness":"muse", "model":model, "thinking":null, "output_contract":{"kind":"string"}});
                json!([{"type":"function_call", "call_id":format!("delegate-{ordinal}"),
                    "name":"exec", "arguments":json!({"input":format!("text(await tools.spawn_agent({args}));")}).to_string()}])
            }
            _ => {
                json!([{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Muse delegation unavailable"}]}])
            }
        }
    } else if body["model"]
        .as_str()
        .is_some_and(|model| model.starts_with("muse-"))
        && last_user.contains("[echo]")
        && has_exec
    {
        json!([{"type":"function_call","call_id":format!("muse-call-{ordinal}"),"name":"exec","arguments":json!({"input":"text(await tools.runtime_echo({text: 'ping'}));"}).to_string()}])
    } else if last_user.contains("[submit]") && has_exec {
        if body["model"]
            .as_str()
            .is_some_and(|model| model.starts_with("muse-"))
        {
            json!([{"type":"function_call","call_id":format!("muse-call-{ordinal}"),"name":"exec","arguments":json!({"input":SUBMIT}).to_string()}])
        } else {
            json!([{"type":"custom_tool_call","call_id":format!("codex-call-{ordinal}"),"name":"exec","input":SUBMIT}])
        }
    } else if last_user.contains("[echo]") && declares(body, "runtime_echo") {
        json!([{"type":"function_call","call_id":format!("codex-call-{ordinal}"),"name":"runtime_echo","arguments":"{\"text\":\"ping\"}"}])
    } else {
        json!([{"type":"message","role":"assistant","content":[{"type":"output_text","text":"native done"}]}])
    };
    let frame = json!({"type":"response.completed","response":{"id":format!("response-{ordinal}"),"status":"completed","output":output}});
    format!("data: {frame}\n\ndata: [DONE]\n\n")
}

fn messages(body: &Value, ordinal: usize) -> String {
    let last = body["messages"].as_array().unwrap().last().unwrap();
    let continuation = last["content"]
        .as_array()
        .is_some_and(|content| content.iter().any(|block| block["type"] == "tool_result"));
    let last_user = last.to_string();
    let call = if continuation {
        None
    } else if last_user.contains("[submit]") && named(&body["tools"], "exec") {
        Some(("exec", json!({"code": SUBMIT})))
    } else if last_user.contains("[echo]") && named(&body["tools"], "runtime_echo") {
        Some(("runtime_echo", json!({"text": "ping"})))
    } else {
        None
    };
    let block = match &call {
        None => json!({"type":"text","text":"native done"}),
        Some((name, _)) => {
            json!({"type":"tool_use","id":format!("claude-call-{ordinal}"),"name":name,"input":{}})
        }
    };
    let mut frames = vec![
        json!({"type":"message_start","message":{"id":format!("message-{ordinal}"),"role":"assistant","model":body["model"],"content":[],"usage":{"input_tokens":10,"output_tokens":0}}}),
        json!({"type":"content_block_start","index":0,"content_block":block}),
    ];
    if let Some((_, input)) = &call {
        frames.push(json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":input.to_string()}}));
    }
    frames.extend([
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":if call.is_some() {"tool_use"} else {"end_turn"}},"usage":{"output_tokens":3}}),
        json!({"type":"message_stop"}),
    ]);
    frames
        .into_iter()
        .map(|frame| format!("data: {frame}\n\n"))
        .collect()
}

async fn serve(transcript: Transcript, trace: PathBuf) -> std::net::SocketAddr {
    let record = move |transport: &'static str, body: &Value| {
        let mut transcript = transcript.lock().unwrap();
        transcript.push(json!({"transport": transport, "request": body}));
        std::fs::write(&trace, serde_json::to_vec_pretty(&*transcript).unwrap()).unwrap();
        transcript.len()
    };
    let codex = record.clone();
    let app = Router::new()
        .route(
            "/responses",
            post(move |Json(body): Json<Value>| {
                let ordinal = codex("responses", &body);
                async move {
                    (
                        [("content-type", "text/event-stream")],
                        responses(&body, ordinal),
                    )
                }
            }),
        )
        .route(
            "/v1/messages",
            post(move |Json(body): Json<Value>| {
                let ordinal = record("messages", &body);
                async move {
                    // A turn that never receives its provider response.
                    let last = &body["messages"].as_array().unwrap().last().unwrap();
                    if last.to_string().contains("[hang]") {
                        std::future::pending::<()>().await;
                    }
                    (
                        [("content-type", "text/event-stream")],
                        messages(&body, ordinal),
                    )
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    address
}

fn last(transcript: &Transcript, transport: &str) -> Value {
    transcript
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|frame| frame["transport"] == transport)
        .unwrap()["request"]
        .clone()
}

#[cfg(feature = "claude")]
fn task(text: &str) -> AgentTask {
    AgentTask {
        role: "synthetic specialist".into(),
        task: text.into(),
        output_schema: json!({"type":"string"}),
    }
}

async fn completed(registry: &Registry, session: &str, id: AgentId) {
    let (summaries, timed_out) = registry.wait(session, &[id], DEADLINE).await.unwrap();
    assert!(!timed_out, "child must finish before deadline");
    let observed = serde_json::to_value(&summaries).unwrap();
    assert!(
        matches!(&summaries[0].status, AgentStatus::Completed { output } if output == "native-result"),
        "{observed}"
    );
}

#[cfg(feature = "claude")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn runtime_spawns_isolated_roots_routes_children_resumes_and_shuts_down() {
    tokio::time::timeout(DEADLINE * 6, journey())
        .await
        .expect("runtime journey deadline");
}

#[cfg(feature = "claude")]
async fn journey() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let evidence =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../output/library-runtime");
    std::fs::create_dir_all(&evidence).unwrap();
    let trace = evidence.join(format!("journey-{}.json", std::process::id()));
    println!("TRANSCRIPT {}", trace.display());
    let transcript: Transcript = Arc::default();
    let address = serve(Arc::clone(&transcript), trace).await;
    let openai = OpenAi::builder("synthetic-key")
        .transport(ResponsesTransport::Https)
        .store(false)
        .api_base_url(format!("http://{address}"))
        .build()
        .unwrap();
    let claude = ClaudeClient::new(
        reqwest::Client::new(),
        format!("http://{address}/v1/messages"),
        "synthetic-key",
    );

    // Backend and selection errors are reported before any provider request.
    assert!(matches!(
        Nanocodex::builder().build(),
        Err(NanocodexError::InvalidRequest(_))
    ));
    assert!(matches!(
        Nanocodex::builder()
            .claude(claude.clone())
            .harness(HarnessFamily::Codex)
            .build(),
        Err(NanocodexError::InvalidRequest(_))
    ));
    assert!(matches!(
        Nanocodex::builder()
            .claude(claude.clone())
            .model(ClaudeModel::Haiku45)
            .thinking(Thinking::High)
            .build(),
        Err(NanocodexError::InvalidRequest(_))
    ));
    let claude_only = Nanocodex::builder().claude(claude.clone()).build().unwrap();
    assert_eq!(claude_only.harness(), HarnessFamily::Claude);
    assert_eq!(claude_only.model(), HarnessFamily::Claude.default_model());
    assert!(matches!(
        claude_only.spawn().model(Model::Sol).build().await,
        Err(NanocodexError::InvalidRequest(_))
    ));
    assert!(transcript.lock().unwrap().is_empty());
    println!(
        "BACKEND_ERRORS no backend, unconfigured family, unsupported thinking rejected; Claude-only defaults to {}",
        claude_only.model()
    );

    // One runtime, shared clients, per-harness tools with one subagent registry.
    let (registry, _control, _updates) = channel(4);
    let handles: Handles = Arc::default();
    let nano = Nanocodex::builder()
        .openai(CodexHarness::new(openai.clone()).tools_factory({
            let (handles, registry) = (Arc::clone(&handles), Arc::clone(&registry));
            move |handle| Ok(registry_tools(&handles, handle, Arc::clone(&registry)))
        }))
        .claude(ClaudeHarness::new(claude.clone()).tools_factory({
            let (handles, registry) = (Arc::clone(&handles), Arc::clone(&registry));
            move |handle| Ok(registry_tools(&handles, handle, Arc::clone(&registry)))
        }))
        .instructions("Runtime instructions: cobalt.")
        .build()
        .unwrap();
    assert_eq!(nano.model(), HarnessModel::Codex(Model::Sol));
    assert_eq!(nano.thinking(), Model::Sol.default_thinking());

    let (codex_root, _events) = nano
        .spawn()
        .instructions("Spawn instructions: amber.")
        .build()
        .await
        .unwrap();
    let (claude_root, _events) = nano
        .spawn()
        .model(ClaudeModel::Sonnet55)
        .thinking(Thinking::Low)
        .build()
        .await
        .unwrap();
    assert_eq!(codex_root.harness_family(), HarnessFamily::Codex);
    assert_eq!(claude_root.harness_family(), HarnessFamily::Claude);
    assert_ne!(codex_root.session_id(), claude_root.session_id());
    let done = codex_root
        .prompt("hello codex root")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    assert_eq!(done.final_message(), "native done");
    let codex_request = last(&transcript, "responses");
    assert_eq!(codex_request["model"], Model::Sol.as_str());
    assert!(
        codex_request["input"]
            .to_string()
            .contains("Spawn instructions: amber.")
    );
    assert!(!codex_request.to_string().contains("cobalt"));
    claude_root
        .prompt("hello claude root")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let claude_request = last(&transcript, "messages");
    assert_eq!(claude_request["model"], ClaudeModel::Sonnet55.as_str());
    assert!(
        claude_request["system"]
            .to_string()
            .contains("Runtime instructions: cobalt.")
    );
    assert_eq!(claude_request["output_config"]["effort"], "low");
    assert!(named(&claude_request["tools"], "exec") && named(&claude_request["tools"], "wait"));
    assert!(!claude_request.to_string().contains("hello codex root"));
    println!(
        "TWO_ROOTS Codex(Sol, spawn instructions) and Claude(Sonnet 5.5, Low, runtime instructions, bridged exec/wait) are isolated"
    );

    let before = transcript.lock().unwrap().len();
    assert!(matches!(
        nano.spawn()
            .harness(HarnessFamily::Claude)
            .model(Model::Sol)
            .build()
            .await,
        Err(NanocodexError::InvalidRequest(_))
    ));
    assert!(matches!(
        nano.spawn()
            .model(ClaudeModel::Haiku45)
            .thinking(Thinking::High)
            .build()
            .await,
        Err(NanocodexError::InvalidRequest(_))
    ));
    assert_eq!(before, transcript.lock().unwrap().len());

    // Model-initiated children route across families through the runtime's
    // recipes; a model-only selection infers its family.
    let codex_owner = handles.lock().unwrap()[codex_root.session_id()].clone();
    let claude_child = start_agent_with(
        &codex_owner,
        &registry,
        codex_root.session_id(),
        task("[submit] Remember amber-constraint."),
        SpawnOptions::new().harness_model(HarnessModel::Claude(ClaudeModel::Sonnet55)),
    )
    .await
    .unwrap();
    completed(&registry, codex_root.session_id(), claude_child.agent_id).await;
    let child_request = last(&transcript, "messages");
    assert!(child_request.to_string().contains("amber-constraint"));
    assert!(
        child_request["system"]
            .to_string()
            .contains("Runtime instructions: cobalt.")
    );
    let claude_owner = handles.lock().unwrap()[claude_root.session_id()].clone();
    let codex_child = start_agent_with(
        &claude_owner,
        &registry,
        claude_root.session_id(),
        task("[submit] A Codex specialist."),
        SpawnOptions::new().harness(HarnessFamily::Codex),
    )
    .await
    .unwrap();
    completed(&registry, claude_root.session_id(), codex_child.agent_id).await;
    println!(
        "CHILD_ROUTING Codex root -> Claude child (model-only) and Claude root -> Codex child completed through runtime recipes"
    );

    // Resume reattaches the runtime's credentials and tools to the session.
    let saved = claude_root.checkpoint().await.unwrap().to_json().unwrap();
    let session_id = claude_root.session_id().to_owned();
    claude_root.shutdown().await.unwrap();
    let (resumed, _events) = nano
        .resume(SessionCheckpoint::from_json(&saved).unwrap())
        .await
        .unwrap();
    assert_eq!(resumed.session_id(), session_id);
    resumed
        .prompt("recall after resume")
        .await
        .unwrap()
        .result()
        .await
        .unwrap();
    let recall = last(&transcript, "messages");
    assert!(recall.to_string().contains("hello claude root"));
    println!("RESUME Claude root resumed with identity {session_id} and replayed history");

    // The runtime owns agents whose handles were dropped until shutdown.
    let (dropped, _events) = nano.spawn().build().await.unwrap();
    let dropped_ref = dropped.downgrade();
    drop(dropped);
    assert!(dropped_ref.is_live());
    let codex_ref = codex_root.downgrade();
    nano.shutdown().await.unwrap();
    assert!(!dropped_ref.is_live());
    assert!(codex_ref.is_live(), "the caller still holds this handle");
    assert!(codex_root.prompt("after shutdown").await.is_err());
    assert!(resumed.prompt("after shutdown").await.is_err());
    assert!(matches!(
        nano.spawn().build().await,
        Err(NanocodexError::AgentStopped)
    ));
    assert!(matches!(
        nano.resume(SessionCheckpoint::from_json(&saved).unwrap())
            .await,
        Err(NanocodexError::AgentStopped)
    ));
    nano.shutdown().await.unwrap();
    println!(
        "SHUTDOWN joined owned roots (including a dropped handle) and routed children; later spawn/resume rejected"
    );

    // Direct tool mode is real on both harnesses.
    let direct = Nanocodex::builder()
        .openai(
            CodexHarness::new(openai)
                .code_mode(false)
                .tools(echo_tools()),
        )
        .claude(
            ClaudeHarness::new(claude.clone())
                .code_mode(false)
                .tools(echo_tools()),
        )
        .build()
        .unwrap();
    for (model, transport) in [
        (HarnessModel::Codex(Model::Sol), "responses"),
        (HarnessModel::Claude(ClaudeModel::Sonnet55), "messages"),
    ] {
        let (agent, _events) = direct.spawn().model(model).build().await.unwrap();
        agent
            .prompt("[echo] call the tool directly")
            .await
            .unwrap()
            .result()
            .await
            .unwrap();
        let request = last(&transcript, transport);
        assert!(
            declares(&request, "runtime_echo"),
            "{model} declares the tool directly"
        );
        assert!(
            request.to_string().contains("echo:ping"),
            "{model} returns the direct result"
        );
    }
    println!("DIRECT_TOOLS Codex and Claude called runtime_echo as a direct provider tool");
    let claude_for_barrier = claude;

    direct.shutdown().await.unwrap();

    // A shutdown cancelled while an agent's turn cleanup is still running
    // leaves that agent owned; the next call joins it.
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Semaphore::new(0));
    let barrier = Nanocodex::builder()
        .claude(
            ClaudeHarness::new(claude_for_barrier)
                .tools(echo_tools())
                .configure({
                    let (entered, release) = (Arc::clone(&entered), Arc::clone(&release));
                    move |builder| {
                        let (entered, release) = (Arc::clone(&entered), Arc::clone(&release));
                        Ok(builder.tools_adapter(move |tools| {
                            let (entered, release) = (Arc::clone(&entered), Arc::clone(&release));
                            Ok(tools.adapter_cleanup(move || {
                                let (entered, release) =
                                    (Arc::clone(&entered), Arc::clone(&release));
                                async move {
                                    entered.notify_one();
                                    drop(release.acquire().await);
                                }
                            }))
                        }))
                    }
                }),
        )
        .build()
        .unwrap();
    let (blocked, _events) = barrier.spawn().build().await.unwrap();
    let blocked_ref = blocked.downgrade();
    let turn = blocked
        .prompt("[hang] wait for the provider")
        .await
        .unwrap();
    drop(blocked);
    {
        let first = barrier.shutdown();
        tokio::pin!(first);
        tokio::select! {
            result = &mut first => panic!("shutdown finished before turn cleanup: {result:?}"),
            () = entered.notified() => {}
        }
        // The first shutdown future is dropped here, mid-cleanup.
    }
    assert!(
        blocked_ref.is_live(),
        "a cancelled shutdown keeps ownership"
    );
    assert!(matches!(
        barrier.spawn().build().await,
        Err(NanocodexError::AgentStopped)
    ));
    release.add_permits(Semaphore::MAX_PERMITS);
    barrier.shutdown().await.unwrap();
    // The turn handle itself retains the agent until it is consumed.
    assert!(turn.result().await.is_err(), "the hung turn was cancelled");
    assert!(!blocked_ref.is_live());
    println!(
        "CANCELLED_SHUTDOWN first shutdown dropped during turn cleanup; agent stayed owned, spawn rejected; second shutdown joined it"
    );
}

#[cfg(feature = "muse")]
#[tokio::test]
async fn muse_runtime_uses_common_agents_with_optional_image_generation() {
    use nanocodex::{Agent, Muse, MuseHarness, MuseModel};
    let evidence =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../output/muse-runtime");
    std::fs::create_dir_all(&evidence).unwrap();
    let transcript: Transcript = Arc::default();
    let trace = evidence.join(format!("journey-{}.json", std::process::id()));
    println!("TRANSCRIPT {}", trace.display());
    let address = serve(Arc::clone(&transcript), trace).await;
    let provider = Muse::builder("synthetic-key")
        .api_base_url(format!("http://{address}"))
        .build()
        .unwrap();
    assert!(matches!(
        Nanocodex::builder()
            .muse(provider.clone())
            .model(Model::Sol)
            .build(),
        Err(NanocodexError::InvalidRequest(_))
    ));
    let runtime = Nanocodex::builder()
        .muse(MuseHarness::new(provider.clone()).tools(echo_tools()))
        .build()
        .unwrap();
    for model in MuseModel::ALL {
        let (agent, _) = runtime.spawn().model(model).build().await.unwrap();
        let agent: Agent = agent;
        assert_eq!(agent.harness_family(), HarnessFamily::Muse);
        let first = agent.prompt("[echo]").await.unwrap().await.unwrap();
        assert_eq!(first.final_message(), "native done");
        let body = last(&transcript, "responses");
        assert_eq!(body["model"], model.as_str());
        assert_eq!(body["store"], false);
        assert!(body.to_string().contains("echo:ping"));
        assert!(!body.to_string().contains("image_gen__imagegen"));
        let checkpoint = first.checkpoint().unwrap();
        assert_eq!(checkpoint.family(), HarnessFamily::Muse);
        let checkpoint =
            serde_json::from_str(&serde_json::to_string(&checkpoint).unwrap()).unwrap();
        let (resumed, _) = runtime.resume(checkpoint).await.unwrap();
        assert_eq!(resumed.session_id(), agent.session_id());
        assert_eq!(
            resumed
                .prompt("Continue.")
                .await
                .unwrap()
                .await
                .unwrap()
                .final_message(),
            "native done"
        );
        let (child, _) = agent.spawn().await.unwrap();
        assert_eq!(
            child
                .prompt("Child.")
                .await
                .unwrap()
                .await
                .unwrap()
                .final_message(),
            "native done"
        );
        assert_eq!(last(&transcript, "responses")["model"], model.as_str());
    }
    runtime.shutdown().await.unwrap();
    assert!(matches!(
        runtime.spawn().build().await,
        Err(NanocodexError::AgentStopped)
    ));
    let opted_in = Nanocodex::builder()
        .muse(
            MuseHarness::new(provider)
                .tools(Tools::builder().image_generation(true).build().unwrap()),
        )
        .build()
        .unwrap();
    let (agent, _) = opted_in.spawn().build().await.unwrap();
    assert_eq!(
        agent
            .prompt("Say hello.")
            .await
            .unwrap()
            .await
            .unwrap()
            .final_message(),
        "native done"
    );
    assert!(
        last(&transcript, "responses")
            .to_string()
            .contains("image_gen__imagegen")
    );
    let openai = OpenAi::builder("synthetic-key")
        .transport(ResponsesTransport::Https)
        .store(false)
        .api_base_url(format!("http://{address}"))
        .build()
        .unwrap();
    let handles: Handles = Arc::default();
    let mixed = Nanocodex::builder()
        .openai(CodexHarness::new(openai).tools_factory({
            let handles = Arc::clone(&handles);
            move |handle: AgentHandle| {
                handles
                    .lock()
                    .unwrap()
                    .insert(handle.session_id().to_owned(), handle);
                Ok(echo_tools())
            }
        }))
        .muse(
            Muse::builder("synthetic-key")
                .api_base_url(format!("http://{address}"))
                .build()
                .unwrap(),
        )
        .build()
        .unwrap();
    for model in [Model::Sol, Model::MuseWonder10] {
        let (agent, _) = mixed.spawn().model(model).build().await.unwrap();
        assert_eq!(
            agent
                .prompt("Mixed runtime.")
                .await
                .unwrap()
                .await
                .unwrap()
                .final_message(),
            "native done"
        );
        assert_eq!(last(&transcript, "responses")["model"], model.as_str());
    }
    let (codex, _) = mixed.spawn().build().await.unwrap();
    let (muse_child, _) = codex
        .spawn_with(SpawnOptions::new().harness_model(MuseModel::Wonder10.into()))
        .await
        .unwrap();
    assert_eq!(muse_child.harness_family(), HarnessFamily::Muse);
    let completed = muse_child
        .prompt("Muse specialist.")
        .await
        .unwrap()
        .await
        .unwrap();
    let checkpoint = completed.checkpoint().unwrap();
    let identity = muse_child.session_id().to_owned();
    muse_child.shutdown().await.unwrap();
    let owner = handles.lock().unwrap()[codex.session_id()].clone();
    let (restored, _) = owner.restore_runtime(checkpoint, None).await.unwrap();
    assert_eq!(restored.session_id(), identity);
    assert_eq!(restored.harness_family(), HarnessFamily::Muse);
    assert_eq!(
        restored
            .prompt("Continue specialist.")
            .await
            .unwrap()
            .await
            .unwrap()
            .final_message(),
        "native done"
    );
    assert_eq!(
        last(&transcript, "responses")["model"],
        MuseModel::Wonder10.as_str()
    );
    let (codex_child, _) = restored
        .spawn_with(SpawnOptions::new().model(Model::Sol))
        .await
        .unwrap();
    assert_eq!(codex_child.harness_family(), HarnessFamily::Codex);
    assert_eq!(
        codex_child
            .prompt("Codex descendant.")
            .await
            .unwrap()
            .await
            .unwrap()
            .final_message(),
        "native done"
    );
    assert_eq!(last(&transcript, "responses")["model"], Model::Sol.as_str());
    mixed.shutdown().await.unwrap();
    opted_in.shutdown().await.unwrap();
}

#[cfg(all(feature = "muse", feature = "durability"))]
#[tokio::test]
async fn muse_durable_runtime_replays_restores_and_branches() {
    use nanocodex::durability::{BranchPoint, MemoryStore, SessionRecord, SessionStore};
    use nanocodex::{Agent, DurableAgentExt, Muse, MuseHarness, MuseModel, PromptRequest};

    let evidence =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../output/muse-durability");
    std::fs::create_dir_all(&evidence).unwrap();
    let transcript: Transcript = Arc::default();
    let trace = evidence.join(format!("journey-{}.json", std::process::id()));
    println!("TRANSCRIPT {}", trace.display());
    let address = serve(Arc::clone(&transcript), trace).await;
    let provider = Muse::builder("synthetic-key")
        .api_base_url(format!("http://{address}"))
        .build()
        .unwrap();
    let store = SessionStore::new(MemoryStore::new().unwrap()).unwrap();
    let identity = nanocodex::agent::session::SessionId::default().to_string();
    let state = store
        .session(SessionRecord::root(
            &identity,
            MuseModel::Spark13.into(),
            None,
        ))
        .await
        .unwrap();
    let builder = Agent::builder(provider.clone())
        .thinking(Thinking::High)
        .tools(echo_tools())
        .durability(state)
        .await
        .unwrap();
    let (agent, _) = builder.clone().build().unwrap();
    assert!(matches!(
        builder.build(),
        Err(NanocodexError::InvalidExecutionPolicy(_))
    ));
    assert_eq!(agent.session_id(), identity);
    let prompt = || PromptRequest::new("[echo]").request_id("first");
    let result = agent.prompt(prompt()).await.unwrap().await.unwrap();
    assert_eq!(result.final_message(), "native done");
    let usage = result.usage().cloned();
    agent.shutdown().await.unwrap();
    let calls = transcript.lock().unwrap().len();
    let stored = store.load(&identity).await.unwrap();
    assert_eq!(stored.turns.len(), 1);
    assert!(stored.transcript.iter().any(|item| matches!(
        item, nanocodex::durability::TranscriptItem::Tool { name, .. } if name == "exec"
    )));
    assert!(
        stored
            .checkpoint
            .as_ref()
            .unwrap()
            .to_string()
            .contains("echo:ping")
    );
    std::fs::write(
        evidence.join("stored-session.json"),
        serde_json::to_string_pretty(&stored.checkpoint).unwrap(),
    )
    .unwrap();
    let checkpoint = stored.session_checkpoint().unwrap().unwrap();
    assert_eq!(checkpoint.family(), HarnessFamily::Muse);
    assert_eq!(checkpoint.thinking(), Thinking::High);
    let recipe = || {
        Nanocodex::builder()
            .muse(MuseHarness::new(provider.clone()).tools(echo_tools()))
            .build()
            .unwrap()
    };
    let runtime = recipe();
    let (resumed, _) = runtime.open(&store, &identity).await.unwrap();
    let replay = resumed.prompt(prompt()).await.unwrap().await.unwrap();
    assert_eq!(replay.final_message(), "native done");
    assert_eq!(replay.usage(), usage.as_ref());
    assert_eq!(
        transcript.lock().unwrap().len(),
        calls,
        "receipt replay makes no HTTP call"
    );
    assert!(
        resumed
            .prompt(PromptRequest::new("different input").request_id("first"))
            .await
            .is_err()
    );
    assert_eq!(transcript.lock().unwrap().len(), calls);
    resumed
        .prompt(PromptRequest::new("Continue durable history.").request_id("second"))
        .await
        .unwrap()
        .await
        .unwrap();
    assert!(
        last(&transcript, "responses")
            .to_string()
            .contains("echo:ping")
    );
    resumed.compact().await.unwrap();
    let compacted = store.load(&identity).await.unwrap();
    assert!(
        compacted
            .checkpoint
            .as_ref()
            .unwrap()
            .to_string()
            .contains("<muse_context_summary>")
    );
    std::fs::write(
        evidence.join("compacted-session.json"),
        serde_json::to_string_pretty(&compacted.checkpoint).unwrap(),
    )
    .unwrap();
    let (child, _) = resumed
        .spawn_with(SpawnOptions::new().model(MuseModel::Wonder10.into()))
        .await
        .unwrap();
    let child_id = child.session_id().to_owned();
    child.prompt("Durable child.").await.unwrap().await.unwrap();
    assert_eq!(
        last(&transcript, "responses")["model"],
        MuseModel::Wonder10.as_str()
    );
    runtime.shutdown().await.unwrap();

    let branch = store
        .branch(&identity, BranchPoint::Through("first".into()), None)
        .await
        .unwrap();
    let branch_id = branch.record.session_id.clone();
    drop(branch);
    let reopened = recipe();
    let (root, _) = reopened.open(&store, &identity).await.unwrap();
    root.prompt("Continue after durable compaction.")
        .await
        .unwrap()
        .await
        .unwrap();
    assert!(
        last(&transcript, "responses")
            .to_string()
            .contains("<muse_context_summary>")
    );
    let (child, _) = reopened.open(&store, &child_id).await.unwrap();
    assert_eq!(child.harness_family(), HarnessFamily::Muse);
    child
        .prompt("Reopened child.")
        .await
        .unwrap()
        .await
        .unwrap();
    assert_eq!(
        last(&transcript, "responses")["model"],
        MuseModel::Wonder10.as_str()
    );
    let (branch, _) = reopened.open(&store, &branch_id).await.unwrap();
    branch
        .prompt("Branch from first turn.")
        .await
        .unwrap()
        .await
        .unwrap();
    let body = last(&transcript, "responses").to_string();
    assert!(body.contains("echo:ping"));
    assert!(!body.contains("Continue durable history."));
    assert_eq!(
        branch.session().lineage.parent_session_id.as_deref(),
        Some(identity.as_str())
    );
    reopened.shutdown().await.unwrap();

    let native: nanocodex::muse::session::SessionSnapshot =
        serde_json::from_value(stored.checkpoint.unwrap()).unwrap();
    assert!(matches!(
        Agent::builder(provider.clone())
            .resume(checkpoint)
            .unwrap()
            .durability(store.resume(&identity).await.unwrap())
            .await,
        Err(NanocodexError::InvalidCheckpoint(_))
    ));
    let latest = store
        .load(&identity)
        .await
        .unwrap()
        .session_checkpoint()
        .unwrap()
        .unwrap();
    let (configured, _) = Agent::builder(provider.clone())
        .resume(latest)
        .unwrap()
        .durability(store.resume(&identity).await.unwrap())
        .await
        .unwrap()
        .build()
        .unwrap();
    configured.shutdown().await.unwrap();
    let fresh = recipe();
    let (portable, _) = fresh
        .resume(
            nanocodex::muse::session_checkpoint(
                identity.clone(),
                stored.summary.record.lineage,
                Thinking::Low,
                native,
            )
            .unwrap(),
        )
        .await
        .unwrap();
    portable
        .prompt("Portable restore.")
        .await
        .unwrap()
        .await
        .unwrap();
    fresh.shutdown().await.unwrap();
    println!(
        "MUSE_DURABILITY receipt replay without HTTP; conflict rejected; native and portable restore; durable summary, child model and historical branch retained"
    );
}

#[cfg(feature = "muse")]
#[tokio::test]
async fn muse_subagent_tools_advertise_and_route_every_version() {
    use nanocodex::{Muse, MuseHarness, MuseModel};
    let evidence =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../output/muse-sdk-gates");
    std::fs::create_dir_all(&evidence).unwrap();
    let transcript: Transcript = Arc::default();
    let trace = evidence.join(format!("delegation-{}.json", std::process::id()));
    println!("TRANSCRIPT {}", trace.display());
    let address = serve(Arc::clone(&transcript), trace).await;
    let provider = Muse::builder("synthetic-key")
        .api_base_url(format!("http://{address}"))
        .build()
        .unwrap();
    let (registry, _control, _updates) = channel(4);
    let handles: Handles = Arc::default();
    let runtime = Nanocodex::builder()
        .muse(MuseHarness::new(provider).tools_factory({
            let (registry, handles) = (Arc::clone(&registry), Arc::clone(&handles));
            move |handle| Ok(registry_tools(&handles, handle, Arc::clone(&registry)))
        }))
        .build()
        .unwrap();
    let (parent, _) = runtime.spawn().build().await.unwrap();
    for model in MuseModel::ALL {
        let result = parent
            .prompt(format!("[delegate-muse] Select {}.", model.as_str()))
            .await
            .unwrap()
            .await
            .unwrap();
        assert_eq!(result.final_message(), "native done");
        let entries = registry
            .directory(parent.session_id(), true, false)
            .await
            .unwrap();
        let child = entries.iter().max_by_key(|entry| entry.agent_id).unwrap();
        completed(&registry, parent.session_id(), child.agent_id).await;
        let requests = transcript.lock().unwrap();
        assert!(requests.iter().any(|frame| {
            frame["request"]["model"] == model.as_str()
                && frame["request"]
                    .to_string()
                    .contains("[submit] Complete the assignment.")
        }));
    }
    assert_eq!(
        registry
            .directory(parent.session_id(), true, false)
            .await
            .unwrap()
            .len(),
        MuseModel::ALL.len()
    );
    runtime.shutdown().await.unwrap();
    println!(
        "MUSE_DELEGATION every advertised version selected through the live spawn tool, routed to its native provider, and completed its output contract"
    );
}
