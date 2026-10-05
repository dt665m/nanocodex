#![cfg(feature = "openai")]
#![allow(missing_docs)]

use eyre::{Result, eyre};
use nanocodex_muse::{Model, Muse, Nanocodex, Thinking, Tools, tools::ToolExposure};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};

async fn request(listener: &TcpListener) -> Result<(TcpStream, Value)> {
    let (mut stream, _) = timeout(Duration::from_secs(10), listener.accept()).await??;
    let mut bytes = Vec::new();
    let end = loop {
        if let Some(i) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        if stream.read_buf(&mut bytes).await? == 0 {
            return Err(eyre!("request ended before headers"));
        }
    };
    let headers = String::from_utf8(bytes[..end].to_vec())?;
    assert!(
        headers.starts_with("POST /v1/responses HTTP/1.1"),
        "unexpected transport or endpoint"
    );
    assert!(!headers.to_lowercase().contains("upgrade: websocket"));
    assert!(headers.to_lowercase().contains("x-api-version: 1.0.0"));
    assert!(
        !headers
            .to_lowercase()
            .contains("x-openai-internal-codex-responses-lite")
    );
    let len: usize = headers
        .lines()
        .find_map(|s| {
            s.to_lowercase()
                .strip_prefix("content-length:")
                .map(|s| s.trim().to_owned())
        })
        .ok_or_else(|| eyre!("missing content length"))?
        .parse()?;
    while bytes.len() < end + len {
        if stream.read_buf(&mut bytes).await? == 0 {
            return Err(eyre!("request body ended early"));
        }
    }
    let body: Value = serde_json::from_slice(&bytes[end..end + len])?;
    assert!(matches!(
        body["model"].as_str(),
        Some("muse-spark-1.3" | "muse-spark-1.3-contributor")
    ));
    assert_eq!(body["store"], false);
    assert_eq!(body["stream"], true);
    assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
    assert_eq!(body["truncation"], "disabled");
    for key in [
        "previous_response_id",
        "client_metadata",
        "type",
        "service_tier",
    ] {
        assert!(body.get(key).is_none());
    }
    for item in body["input"].as_array().unwrap() {
        assert_ne!(item["type"], "additional_tools");
        assert_ne!(item["type"], "compaction_trigger");
    }
    Ok((stream, body))
}

fn answer(text: &str) -> Value {
    // Meta's final answers have no phase or end_turn fields.
    json!({"type":"message", "role":"assistant", "content":[{"type":"output_text", "text":text}]})
}

async fn respond(mut stream: TcpStream, id: &str, output: Vec<Value>, tokens: u64) -> Result<()> {
    let event = json!({"type":"response.completed", "response":{"id":id, "status":"completed", "output":output,
        "usage":{"input_tokens":tokens, "output_tokens":2, "total_tokens":tokens + 2}}});
    let body = format!("data: {event}\n\ndata: [DONE]\n\n");
    stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await?;
    stream.shutdown().await?;
    Ok(())
}

async fn turn(agent: &Nanocodex, prompt: &str) -> Result<String> {
    let result = timeout(Duration::from_secs(10), async {
        agent.prompt(prompt).await?.result().await
    })
    .await??;
    Ok(result.final_message().to_owned())
}

#[tokio::test]
async fn malformed_custom_tool_wrappers_fail_without_panicking_the_agent() -> Result<()> {
    for arguments in ["[]", "1", "true", "\"text\"", "{}", "{\"input\":42}"] {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let endpoint = format!("http://{}/v1", listener.local_addr()?);
        let server = tokio::spawn(async move {
            let (stream, catalog) = request(&listener).await?;
            let name = catalog["tools"]
                .as_array()
                .unwrap()
                .iter()
                .find(|tool| tool["name"] == "exec")
                .unwrap()["name"]
                .clone();
            respond(stream, "malformed", vec![json!({"type":"function_call","call_id":"invalid-wrapper","name":name,"arguments":arguments})], 12).await?;
            Result::<()>::Ok(())
        });
        let provider = Muse::builder("synthetic-test-key")
            .api_base_url(endpoint)
            .build()?;
        let tools = Tools::builder()
            .without_defaults()
            .exposure(ToolExposure::CodeModeOnly)
            .build()?;
        let (agent, _) = Nanocodex::builder(provider).tools(tools).build()?;
        let result = turn(&agent, "Run a tool.").await;
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("Muse custom-tool wrapper")
        );
        // The private driver must remain alive and accept shutdown after protocol failure.
        timeout(Duration::from_secs(10), agent.shutdown()).await??;
        server.await??;
    }
    Ok(())
}

#[tokio::test]
async fn muse_edits_a_file_compacts_and_continues_with_authentic_tool_receipts() -> Result<()> {
    file_edit_compaction_journey(Model::Muse).await
}

#[tokio::test]
async fn contributor_uses_its_model_id_for_tools_compaction_and_continuation() -> Result<()> {
    assert!(
        Muse::builder("synthetic-test-key")
            .model(Model::MuseContributor)
            .thinking(Thinking::Max)
            .build()
            .is_err()
    );
    file_edit_compaction_journey(Model::MuseContributor).await
}

async fn file_edit_compaction_journey(model: Model) -> Result<()> {
    let workspace = tempfile::tempdir()?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}/v1", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, first) = request(&listener).await?;
        assert_eq!(first["model"], model.as_str());
        assert_eq!(first["reasoning"]["effort"], "low");
        let patch_tool = first["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| {
                tool["name"]
                    .as_str()
                    .is_some_and(|name| name.ends_with("apply_patch"))
            })
            .ok_or_else(|| eyre!("apply_patch missing"))?;
        let name = patch_tool["name"].as_str().unwrap();
        assert_eq!(patch_tool["type"], "function");
        let arguments =
            json!({"input":"*** Begin Patch\n*** Add File: hello.txt\n+hello Muse\n*** End Patch"})
                .to_string();
        respond(stream, "resp-tool", vec![json!({"type":"reasoning", "id":"rs_original", "summary":[], "encrypted_content":"opaque-original"}),
            json!({"type":"function_call", "call_id":"patch1", "name":name, "arguments":arguments})], 9_000).await?;
        let (stream, compact) = request(&listener).await?;
        assert_eq!(compact["model"], model.as_str());
        assert_eq!(compact["tool_choice"], "none");
        assert_eq!(compact["reasoning"]["effort"], "none");
        assert!(compact.to_string().contains("Summarize the conversation"));
        assert!(
            !compact.to_string().contains("opaque-original"),
            "active suffix was summarized"
        );
        respond(
            stream,
            "resp-summary",
            vec![answer(
                "The user requested hello.txt. Continue the active edit.",
            )],
            20,
        )
        .await?;
        let (stream, continuation) = request(&listener).await?;
        assert_eq!(continuation["model"], model.as_str());
        let items = continuation["input"].as_array().unwrap();
        assert!(continuation.to_string().contains("muse_context_summary"));
        let reasoning = items.iter().find(|i| i["type"] == "reasoning").unwrap();
        assert_eq!(reasoning["encrypted_content"], "opaque-original");
        assert_eq!(reasoning["summary"], json!([]));
        assert_eq!(
            items
                .iter()
                .filter(|i| i["type"] == "function_call" && i["call_id"] == "patch1")
                .count(),
            1
        );
        let receipt = items
            .iter()
            .find(|i| i["type"] == "function_call_output" && i["call_id"] == "patch1")
            .unwrap();
        assert!(receipt.to_string().contains("hello.txt"));
        respond(stream, "resp-answer", vec![answer("Created hello.txt")], 12).await?;
        let (stream, manual) = request(&listener).await?;
        assert_eq!(manual["model"], model.as_str());
        assert_eq!(manual["tool_choice"], "none");
        respond(
            stream,
            "resp-manual",
            vec![answer(
                "hello.txt contains hello Muse. The task is complete.",
            )],
            12,
        )
        .await?;
        let (stream, followup) = request(&listener).await?;
        assert_eq!(followup["model"], model.as_str());
        assert!(followup.to_string().contains("task is complete"));
        assert!(!followup.to_string().contains("opaque-original"));
        assert!(!followup.to_string().contains("patch1"));
        respond(stream, "resp-followup", vec![answer("hello Muse")], 12).await?;
        Result::<()>::Ok(())
    });
    let provider = Muse::builder("synthetic-test-key")
        .model(model)
        .api_base_url(endpoint)
        .context_window_tokens(40_000)
        .build()?;
    let tools = Tools::builder()
        .exposure(ToolExposure::DirectOnly)
        .build()?;
    let (agent, _events) = Nanocodex::builder(provider)
        .workspace(workspace.path())
        .instructions("Complete the user's file task.")
        .tools(tools)
        .build()?;
    assert_eq!(
        turn(&agent, "Create hello.txt containing hello Muse.").await?,
        "Created hello.txt"
    );
    assert_eq!(
        std::fs::read_to_string(workspace.path().join("hello.txt"))?,
        "hello Muse\n"
    );
    timeout(Duration::from_secs(10), agent.compact()).await??;
    assert_eq!(turn(&agent, "What did you create?").await?, "hello Muse");
    agent.shutdown().await?;
    server.await??;
    Ok(())
}

#[tokio::test]
async fn invalid_summary_leaves_history_available_for_retry() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}/v1", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, _) = request(&listener).await?;
        respond(
            stream,
            "resp-one",
            vec![answer("Remember the number 42.")],
            12,
        )
        .await?;
        let (stream, _) = request(&listener).await?;
        respond(stream, "resp-empty", vec![answer(" ")], 12).await?;
        let (stream, retry) = request(&listener).await?;
        assert!(retry.to_string().contains("Remember the number 42"));
        respond(stream, "resp-retry", vec![answer("The number is 42.")], 12).await?;
        let (stream, followup) = request(&listener).await?;
        assert!(followup.to_string().contains("The number is 42"));
        respond(stream, "resp-two", vec![answer("42")], 12).await?;
        Result::<()>::Ok(())
    });
    let provider = Muse::builder("synthetic-test-key")
        .api_base_url(endpoint)
        .build()?;
    let tools = Tools::builder()
        .without_defaults()
        .exposure(ToolExposure::DirectOnly)
        .build()?;
    let (agent, _events) = Nanocodex::builder(provider).tools(tools).build()?;
    turn(&agent, "Remember 42").await?;
    let before = serde_json::to_value(agent.snapshot().await?)?;
    assert!(
        timeout(Duration::from_secs(10), agent.compact())
            .await?
            .is_err()
    );
    let after = serde_json::to_value(agent.snapshot().await?)?;
    assert_eq!(before["history"], after["history"]);
    timeout(Duration::from_secs(10), agent.compact()).await??;
    assert_eq!(turn(&agent, "What number?").await?, "42");
    agent.shutdown().await?;
    server.await??;
    Ok(())
}

#[tokio::test]
async fn muse_discovers_and_dispatches_upstream_mcp_tools() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}/v1", listener.local_addr()?);
    let server = tokio::spawn(async move {
        let (stream, catalog) = request(&listener).await?;
        assert!(
            catalog["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["name"] == "tool_search" && t["type"] == "function")
        );
        respond(stream, "resp-search", vec![json!({"type":"function_call", "call_id":"search1", "name":"tool_search", "arguments":"{\"query\":\"echo deterministic message\",\"limit\":1}"})], 12).await?;
        let (stream, discovered) = request(&listener).await?;
        let tool = discovered["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| {
                t["name"]
                    .as_str()
                    .is_some_and(|name| name.starts_with("mcp__fixture") && name.ends_with("echo"))
            })
            .ok_or_else(|| eyre!("MCP discovery omitted echo"))?;
        assert!(
            discovered["input"]
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["type"] == "function_call_output" && i["call_id"] == "search1")
        );
        respond(stream, "resp-echo", vec![json!({"type":"function_call", "call_id":"echo1", "name":tool["name"], "arguments":"{\"message\":\"hello\"}"})], 12).await?;
        let (stream, executed) = request(&listener).await?;
        assert!(
            executed["input"]
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["type"] == "function_call_output"
                    && i["call_id"] == "echo1"
                    && i.to_string().contains("fixture:hello"))
        );
        respond(stream, "resp-final", vec![answer("fixture:hello")], 12).await?;
        Result::<()>::Ok(())
    });
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/mcp-stdio-server.mjs");
    let mcp = nanocodex_muse::tools::mcp::Mcp::builder()
        .server(
            "fixture",
            nanocodex_muse::tools::mcp::McpServer::stdio("node").arg(fixture.to_string_lossy()),
        )
        .build()?;
    let tools = Tools::builder()
        .exposure(ToolExposure::DirectAndCodeMode)
        .provider(mcp)
        .build()?;
    let provider = Muse::builder("synthetic-test-key")
        .api_base_url(endpoint)
        .build()?;
    let (agent, _events) = Nanocodex::builder(provider).tools(tools).build()?;
    let result = turn(&agent, "Find and call the MCP echo tool.").await;
    agent.shutdown().await?;
    server.await??;
    assert_eq!(result?, "fixture:hello");
    Ok(())
}

#[tokio::test]
async fn muse_cancels_an_inflight_http_response_and_accepts_a_new_turn() -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}/v1", listener.local_addr()?);
    let (started, seen) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = request(&listener).await?;
        started.send(()).map_err(|_| eyre!("test stopped"))?;
        let (next, followup) = request(&listener).await?;
        assert!(followup.to_string().contains("after cancellation"));
        respond(next, "resp-followup", vec![answer("continued")], 12).await?;
        drop(stream);
        Result::<()>::Ok(())
    });
    let provider = Muse::builder("synthetic-test-key")
        .api_base_url(endpoint)
        .build()?;
    let tools = Tools::builder()
        .without_defaults()
        .exposure(ToolExposure::DirectOnly)
        .build()?;
    let (agent, _events) = Nanocodex::builder(provider).tools(tools).build()?;
    let active = agent.prompt("Wait for a response").await?;
    timeout(Duration::from_secs(10), seen).await??;
    active.cancel().await?;
    assert!(
        timeout(Duration::from_secs(10), active.result())
            .await?
            .is_err()
    );
    assert_eq!(
        turn(&agent, "Continue after cancellation").await?,
        "continued"
    );
    agent.shutdown().await?;
    server.await??;
    Ok(())
}
