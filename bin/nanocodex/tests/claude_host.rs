//! Shipped-CLI journeys against synthetic Messages/SSE. No live inference or
//! credentials. Artifacts retain every provider request and CLI stdout/stderr.
use axum::{Json, Router, routing::post};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{net::TcpListener, process::Command};

fn sse(block: Value) -> String {
    let tool = block["type"] == "tool_use";
    let start = if tool {
        json!({"type":"tool_use","id":block["id"],"name":block["name"],"input":{}})
    } else {
        json!({"type":"text","text":""})
    };
    let delta = if tool {
        json!({"type":"input_json_delta","partial_json":block["input"].to_string()})
    } else {
        json!({"type":"text_delta","text":block["text"]})
    };
    [json!({"type":"message_start","message":{"id":"fixture","role":"assistant","model":"claude-sonnet-5-5","content":[],"usage":{"input_tokens":1,"output_tokens":0}}}),
    json!({"type":"content_block_start","index":0,"content_block":start}), json!({"type":"content_block_delta","index":0,"delta":delta}), json!({"type":"content_block_stop","index":0}), json!({"type":"message_delta","delta":{"stop_reason":if tool {"tool_use"} else {"end_turn"}},"usage":{"output_tokens":1}}),json!({"type":"message_stop"})].iter().map(|v|format!("data: {v}\n\n")).collect()
}
fn tool(stage: usize, name: &str, input: Value) -> Value {
    json!({"type":"tool_use","id":format!("call-{stage}"),"name":name,"input":input})
}
fn result(body: &Value) -> Option<&Value> {
    body["messages"]
        .as_array()?
        .iter()
        .rev()
        .filter_map(|m| m["content"].as_array())
        .flatten()
        .find(|block| block["type"] == "tool_result")
}
fn parsed_result(body: &Value) -> Value {
    let result = result(body).unwrap();
    let text = result["content"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| {
            result["content"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|b| b["text"].as_str())
                .collect()
        });
    serde_json::from_str(&text).unwrap_or(json!(text))
}
fn command(workspace: &Path, endpoint: &str) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_nanocodex"));
    cmd.arg("run")
        .current_dir(workspace)
        .env_clear()
        .env("HOME", workspace.join("home"))
        .env("CODEX_HOME", workspace.join("codex-home"))
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("NANOCODEX_COMPUTER", "off")
        .args([
            "--claude",
            "--model",
            "claude-sonnet-5-5",
            "--thinking",
            "medium",
            "--claude-api-key",
            "synthetic-fixture-key",
            "--claude-messages-url",
            endpoint,
            "--rollouts",
            "false",
            "--browser=none",
            "--mcp-defaults",
            "false",
            "--mcp-codex-config",
            "false",
            "--web-search",
            "false",
            "--image-generation",
            "false",
            "--memory",
            "false",
            "--subagents",
            "true",
            "--cwd",
        ])
        .arg(workspace)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    cmd
}
fn artifact(name: &str) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../output/claude-host")
        .join(format!("{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(path.join("workspace/home")).unwrap();
    path
}
#[tokio::test]
async fn native_cli_background_bash_tasks_and_agent_lifecycle() {
    let artifact = artifact("native-host");
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let root_count = Arc::new(Mutex::new(0usize));
    let bash_ids = Arc::new(Mutex::new(Vec::<String>::new()));
    let log = requests.clone();
    let count = root_count.clone();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1/messages", listener.local_addr().unwrap());
    let app = Router::new().route("/v1/messages",post(move |Json(body): Json<Value>| {
        let log = log.clone(); let count = count.clone(); let bash_ids = bash_ids.clone();
        async move {
            let first = body["messages"][0]["content"].to_string();
            let child = first.contains("HOST_CHILD_FIXTURE");
            log.lock().unwrap().push(json!({"child":child,"request":body}));
            let reply = if child {
                if first.contains("HOST_CHILD_SLOW") { tokio::time::sleep(Duration::from_secs(30)).await; }
                if result(&body).is_none() { tool(100,"SubmitResult",json!({"output":"child-complete"})) }
                else { json!({"type":"text","text":"child finished"}) }
            } else {
                let stage = { let mut n = count.lock().unwrap(); let current = *n; *n += 1; current };
                if [1, 4, 25, 26, 30, 36].contains(&stage) { bash_ids.lock().unwrap().push(parsed_result(&body)["task_id"].as_str().unwrap().to_owned()); }
                let ids = bash_ids.lock().unwrap().clone();
                match stage {
                    0 => tool(stage,"Bash",json!({"command":"sleep 0.1; printf background-done; printf retained > background.txt","run_in_background":true})),
                    1 => tool(stage,"TaskOutput",json!({"task_id":ids[0],"block":true,"timeout":10000})),
                    2 => tool(stage,"Bash",json!({"command":"printf failure-output; exit 7"})),
                    3 => tool(stage,"Bash",json!({"command":"sleep 30; printf should-not-run > stopped.txt","run_in_background":true})),
                    4 => tool(stage,"TaskStop",json!({"task_id":ids[1]})),
                    5 => tool(stage,"TaskOutput",json!({"task_id":ids[1],"block":false})),
                    6 => tool(stage,"Bash",json!({"command":"sleep 30; printf should-not-run > timeout.txt","timeout":25})),
                    7 => tool(stage,"TaskOutput",json!({"task_id":"bash-unknown","block":false})),
                    8 => tool(stage,"TaskCreate",json!({"subject":"Real CLI task","description":"Complete the native journey"})),
                    9 => tool(stage,"TaskUpdate",json!({"taskId":"1","status":"completed"})),
                    10 => tool(stage,"TaskGet",json!({"taskId":"1"})),
                    11 => tool(stage,"TodoWrite",json!({"todos":[{"content":"Verify native host","activeForm":"Verifying native host","status":"completed"}]})),
                    12 => tool(stage,"Agent",json!({"description":"CLI child","prompt":"HOST_CHILD_FIXTURE return child-complete","run_in_background":true})),
                    13 => tool(stage,"TaskOutput",json!({"task_id":"agent-1","block":true,"timeout":20000})),
                    14 => tool(stage,"ListAgents",json!({"include_completed":true})),
                    15 => tool(stage,"SendMessage",json!({"recipient":"agent-1","content":"HOST_CHILD_FIXTURE follow-up"})),
                    16 => tool(stage,"Agent",json!({"description":"foreground child","prompt":"HOST_CHILD_FIXTURE return child-complete"})),
                    17 => tool(stage,"TaskStop",json!({"task_id":"agent-2"})),
                    18 => tool(stage,"Agent",json!({"description":"unsupported mode","prompt":"never execute","isolation":"worktree"})),
                    19 => tool(stage,"Bash",json!({"command":"touch invalid-background.txt","run_in_background":true,"timeout":0})),
                    20 => tool(stage,"Agent",json!({"description":"interruptible child","prompt":"HOST_CHILD_FIXTURE HOST_CHILD_SLOW wait for cancellation","run_in_background":true})),
                    21 => tool(stage,"TaskStop",json!({"task_id":"agent-3"})),
                    22 => tool(stage,"TaskOutput",json!({"task_id":"agent-3","block":false})),
                    23 => tool(stage,"Agent",json!({"description":"resume child","prompt":"HOST_CHILD_FIXTURE resume prompt","resume":"agent-1"})),
                    24 => tool(stage,"Bash",json!({"command":"(sleep 0.3; printf should-not-run > independent-stopped.txt) & wait","run_in_background":true})),
                    25 => tool(stage,"Bash",json!({"command":"sleep 0.5; printf independent-done; printf survived > independent.txt","run_in_background":true})),
                    26 => tool(stage,"TaskStop",json!({"task_id":ids[2]})),
                    27 => tool(stage,"TaskOutput",json!({"task_id":ids[3],"block":true,"timeout":10000})),
                    28 => tool(stage,"TaskOutput",json!({"task_id":ids[2],"block":false})),
                    29 => tool(stage,"Bash",json!({"command":"printf background-failure; exit 9","run_in_background":true})),
                    30 => tool(stage,"TaskOutput",json!({"task_id":ids[4],"block":true,"timeout":10000})),
                    31 => tool(stage,"Bash",json!({"command":"mkdir -p cwd/nested; cd cwd; export NANO_FIXTURE_ENV=transient"})),
                    32 => tool(stage,"Bash",json!({"command":"pwd"})),
                    33 => tool(stage,"Bash",json!({"command":"cd nested; (cd ..); exit 6"})),
                    34 => tool(stage,"Bash",json!({"command":"pwd"})),
                    35 => tool(stage,"Bash",json!({"command":"pwd; cd /","run_in_background":true})),
                    36 => tool(stage,"TaskOutput",json!({"task_id":ids[5],"block":true,"timeout":10000})),
                    37 => tool(stage,"Bash",json!({"command":"pwd"})),
                    38 => tool(stage,"Bash",json!({"command":"cd /; pwd"})),
                    39 => tool(stage,"Bash",json!({"command":"pwd"})),
                    40 => tool(stage,"Bash",json!({"command":r#"printf '%s' "${NANO_FIXTURE_ENV-unset}""#})),
                    _ => json!({"type":"text","text":"native-host-journey-complete"}),
                }
            };
            ([("content-type","text/event-stream")],sse(reply))
        }
    }));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let mut cmd = command(&artifact.join("workspace"), &endpoint);
    cmd.arg("Exercise native host tools.");
    std::fs::write(artifact.join("scenario.txt"),format!("Command: cargo +1.97.0 test -p nanocodex-bin --test claude_host native_cli_background_bash_tasks_and_agent_lifecycle\nCLI: {cmd:?}\nExpected: background completion, independent descendant stop with another job surviving beyond its effect deadline, foreground/background nonzero exit receipts, timeout, project-bounded cwd carry/reset, background cwd isolation, nonpersistent exports, durable-board task CRUD, actual child spawn/submit/wait/message, unsupported input errors.\n")).unwrap();
    let output = tokio::time::timeout(Duration::from_secs(70), cmd.output())
        .await
        .expect("CLI journey timed out")
        .unwrap();
    std::fs::write(artifact.join("stdout.txt"), &output.stdout).unwrap();
    std::fs::write(artifact.join("stderr.txt"), &output.stderr).unwrap();
    let requests = requests.lock().unwrap();
    std::fs::write(
        artifact.join("requests.json"),
        serde_json::to_vec_pretty(&*requests).unwrap(),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "{}: {}",
        artifact.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("native-host-journey-complete"));
    let roots: Vec<_> = requests
        .iter()
        .filter(|r| r["child"] == false)
        .map(|r| &r["request"])
        .collect();
    assert_eq!(roots.len(), 42, "inspect {}", artifact.display());
    let names: Vec<_> = roots[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["name"].as_str())
        .collect();
    for name in [
        "Bash",
        "Agent",
        "TaskOutput",
        "TaskStop",
        "ListAgents",
        "CloseAgent",
        "SendMessage",
        "SubmitResult",
        "TaskCreate",
        "TaskGet",
        "TaskList",
        "TaskUpdate",
        "TodoWrite",
    ] {
        assert!(names.contains(&name), "missing {name}");
    }
    for name in ["exec", "wait", "tool_search", "spawn_agent", "exec_command"] {
        assert!(!names.contains(&name), "leaked {name}");
    }
    assert!(
        parsed_result(roots[1])["task_id"]
            .as_str()
            .unwrap()
            .starts_with("bash-")
    );
    assert_eq!(parsed_result(roots[2])["status"], "completed");
    assert_eq!(
        parsed_result(roots[2])["output"]["stdout"],
        "background-done"
    );
    assert_eq!(parsed_result(roots[3])["exit_code"], 7);
    assert_eq!(parsed_result(roots[5])["status"], "stopped");
    assert_eq!(parsed_result(roots[6])["status"], "stopped");
    for stage in [7, 8, 19, 20] {
        assert_eq!(
            result(roots[stage]).unwrap()["is_error"],
            true,
            "stage {stage}"
        );
    }
    assert!(parsed_result(roots[7]).to_string().contains("timed out"));
    assert_eq!(parsed_result(roots[9])["task"]["id"], "1");
    assert_eq!(parsed_result(roots[11])["task"]["status"], "completed");
    assert_eq!(parsed_result(roots[13])["task_id"], "agent-1");
    assert!(
        parsed_result(roots[14])
            .to_string()
            .contains("child-complete"),
        "{}",
        parsed_result(roots[14])
    );
    assert!(parsed_result(roots[15]).to_string().contains("CLI child"));
    assert_ne!(result(roots[16]).unwrap()["is_error"], true);
    assert!(
        parsed_result(roots[17])
            .to_string()
            .contains("child-complete")
    );
    assert_eq!(parsed_result(roots[21])["task_id"], "agent-3");
    assert_ne!(result(roots[22]).unwrap()["is_error"], true);
    assert!(
        parsed_result(roots[23]).to_string().contains("interrupted"),
        "{}",
        parsed_result(roots[23])
    );
    assert_eq!(parsed_result(roots[24])["task_id"], "agent-1");
    assert_ne!(result(roots[24]).unwrap()["is_error"], true);
    let workspace = artifact.join("workspace").canonicalize().unwrap();
    assert_eq!(
        parsed_result(roots[33])["stdout"],
        format!("{}\n", workspace.join("cwd").display())
    );
    assert_eq!(parsed_result(roots[34])["exit_code"], 6);
    for stage in [35, 38] {
        assert_eq!(
            parsed_result(roots[stage])["stdout"],
            format!("{}\n", workspace.join("cwd/nested").display())
        );
    }
    assert_eq!(
        parsed_result(roots[37])["output"]["stdout"],
        format!("{}\n", workspace.join("cwd/nested").display())
    );
    assert_eq!(parsed_result(roots[39])["stdout"], "/\n");
    assert_eq!(
        parsed_result(roots[40])["stdout"],
        format!("{}\n", workspace.display())
    );
    assert_eq!(parsed_result(roots[41])["stdout"], "unset");
    assert_eq!(parsed_result(roots[27])["status"], "stopped");
    assert_eq!(parsed_result(roots[28])["status"], "completed");
    assert_eq!(
        parsed_result(roots[28])["output"]["stdout"],
        "independent-done"
    );
    assert_eq!(parsed_result(roots[29])["status"], "stopped");
    assert_eq!(parsed_result(roots[31])["status"], "completed");
    assert_eq!(parsed_result(roots[31])["output"]["exit_code"], 9);
    assert_eq!(
        parsed_result(roots[31])["output"]["stdout"],
        "background-failure"
    );
    assert_eq!(
        std::fs::read_to_string(artifact.join("workspace/independent.txt")).unwrap(),
        "survived"
    );
    assert!(!artifact.join("workspace/independent-stopped.txt").exists());
    assert_eq!(
        std::fs::read_to_string(artifact.join("workspace/background.txt")).unwrap(),
        "retained"
    );
    for name in ["stopped.txt", "timeout.txt", "invalid-background.txt"] {
        assert!(!artifact.join("workspace").join(name).exists());
    }
    std::fs::write(
        artifact.join("outcome.txt"),
        "PASS: shipped CLI native host lifecycle and failure journey; synthetic provider only.\n",
    )
    .unwrap();
    eprintln!("evidence: {}", artifact.display());
    server.abort();
}

#[test]
fn native_cli_durable_files_media_and_stale_background_tasks() {
    let repository = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = std::process::Command::new("python3")
        .arg(repository.join("scripts/tests/claude-native-cli-journey.py"))
        .args(["--binary", env!("CARGO_BIN_EXE_nanocodex")])
        .current_dir(&repository)
        .output()
        .expect("run native CLI durability journey");
    eprintln!("{}", String::from_utf8_lossy(&output.stdout));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
