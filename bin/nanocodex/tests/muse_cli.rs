//! Real local CLI/device OAuth journeys; only Meta's HTTP endpoints are synthetic.
#[path = "support/local_cli.rs"]
mod local_cli;
use local_cli::local_cli;

use axum::{
    Json, Router,
    body::Bytes,
    http::{HeaderMap, Method, StatusCode, Uri},
    response::IntoResponse,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use nanocodex::{HarnessFamily, MuseModel, durability::SessionStore};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::{Output, Stdio},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::process::Command;

const IDENTITY: &str = "synthetic-muse-identity";
const KEY_ONE: &str = "synthetic-muse-inference-one";
const KEY_TWO: &str = "synthetic-muse-inference-two";
const STATIC_KEY: &str = "synthetic-muse-api-key";

#[derive(Default)]
struct State {
    polls: usize,
    exchanges: usize,
    reject_key: bool,
    deny_login: bool,
    requests: Vec<Value>,
}
struct Fixture {
    workspace: tempfile::TempDir,
    origin: String,
    config: PathBuf,
    evidence: PathBuf,
    state: Arc<Mutex<State>>,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl Fixture {
    async fn new() -> Self {
        let workspace = tempfile::tempdir().unwrap();
        let evidence = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../output/muse-cli")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&evidence).unwrap();
        let state = Arc::new(Mutex::new(State {
            reject_key: true,
            ..State::default()
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let config = workspace.path().join("oauth.json");
        std::fs::write(
            &config,
            json!({"issuer":origin,"api_origin":origin}).to_string(),
        )
        .unwrap();
        let observed = state.clone();
        let artifact = evidence.clone();
        let app = Router::new().fallback(move |method: Method, uri: Uri, headers: HeaderMap, bytes: Bytes| {
            let state = observed.clone();
            let artifact = artifact.clone();
            async move {
                let mut state = state.lock().unwrap();
                // Never retain identity, inference or device credentials in evidence.
                state.requests.push(json!({"method":method.as_str(), "path":uri.path()}));
                let response = match uri.path() {
                    "/oidc/device/authorization/" => {
                        assert_eq!(method, Method::POST);
                        assert!(std::str::from_utf8(&bytes).unwrap().contains("client_id="));
                        Json(json!({"device_code":"synthetic-device", "user_code":"TEST-CODE", "verification_uri":"https://auth.meta.com/oauth/device/", "verification_uri_complete":"https://auth.meta.com/oauth/device/?code=TEST-CODE", "interval":1,"expires_in":30})).into_response()
                    }
                    "/oidc/device/token/" => {
                        assert_eq!(method, Method::POST);
                        assert!(std::str::from_utf8(&bytes).unwrap().contains("device_code=synthetic-device"));
                        state.polls += 1;
                        if state.deny_login {
                            (StatusCode::BAD_REQUEST, Json(json!({"error":"access_denied"}))).into_response()
                        } else if state.polls == 1 {
                            (StatusCode::BAD_REQUEST, Json(json!({"error":"authorization_pending"}))).into_response()
                        } else { Json(json!({"access_token":IDENTITY})).into_response() }
                    }
                    "/muse-code/key" => {
                        assert_eq!(headers["authorization"], format!("Bearer {IDENTITY}"));
                        let body: Value = serde_json::from_slice(&bytes).unwrap();
                        assert_eq!(body, if state.exchanges == 0 { json!({"onboard":true}) } else { json!({}) });
                        state.exchanges += 1;
                        Json(json!({"api_key": if state.exchanges == 1 {KEY_ONE} else {KEY_TWO}, "is_subs_active":true})).into_response()
                    }
                    "/v1/responses" => {
                        let authorization = headers["authorization"].to_str().unwrap();
                        assert!([KEY_ONE, KEY_TWO, STATIC_KEY].iter().any(|key| authorization == format!("Bearer {key}")));
                        assert!(!headers.contains_key("chatgpt-account-id"));
                        let body: Value = serde_json::from_slice(&bytes).unwrap();
                        state.requests.last_mut().unwrap()["body"] = body.clone();
                        assert!(body.get("previous_response_id").is_none());
                        let tools = body["tools"].as_array().unwrap();
                        assert!(tools.iter().any(|tool| tool["name"] == "exec"));
                        let catalog = tools.iter().find(|tool| tool["name"] == "exec").unwrap()["description"].as_str().unwrap();
                        assert!(!catalog.contains("imagegen"), "image generation must be opt-in");
                        if state.reject_key && authorization == format!("Bearer {KEY_ONE}") {
                            state.reject_key = false;
                            (StatusCode::UNAUTHORIZED, Json(json!({"error":{"message":KEY_ONE}}))).into_response()
                        } else {
                            let input = body["input"].as_array().unwrap();
                            let continuation = input.last().is_some_and(|item| item["type"] == "function_call_output");
                            let text = input.iter().rev().find(|item| item["role"] == "user").unwrap().to_string();
                            let output = if text.contains("[effect]") && !continuation {
                                json!([{"type":"function_call","call_id":"effect","name":"exec","arguments":json!({"input":"text(await tools.exec_command({cmd: \"printf muse-effect > effect.txt\"}));"}).to_string()}])
                            } else if text.contains("[delegate]") && !continuation {
                                assert!(catalog.contains("muse-wonder-1.0"));
                                let code = "const child = await tools.spawn_agent({role: 'Muse worker', task: '[child-effect] Write the child file.', harness: 'muse', model: 'muse-wonder-1.0', thinking: null, output_contract: {kind: 'string'}}); text(await tools.wait_agent({agent_ids: [child.agent_id], timeout_ms: 10000}));";
                                json!([{"type":"function_call","call_id":"delegate","name":"exec","arguments":json!({"input":code}).to_string()}])
                            } else if text.contains("[child-effect]") && !continuation {
                                json!([{"type":"function_call","call_id":"child-effect","name":"exec","arguments":json!({"input":"text(await tools.exec_command({cmd: \"printf muse-child > child.txt\"}));"}).to_string()}])
                            } else { json!([{"type":"message","role":"assistant","content":[{"type":"output_text","text": if text.contains("[resume]") {"muse-resumed-ok"} else {"muse-cli-ok"}}]}]) };
                            let frame = json!({"type":"response.completed", "response":{"id":format!("fixture-{}",state.requests.len()),"status":"completed","output":output,"usage":{"input_tokens":10,"output_tokens":5}}});
                            ([("content-type", "text/event-stream")], format!("data: {frame}\n\ndata: [DONE]\n\n")).into_response()
                        }
                    }
                    _ => panic!("unexpected Meta route {method} {uri}"),
                };
                std::fs::write(artifact.join("http.json"), serde_json::to_vec_pretty(&state.requests).unwrap()).unwrap();
                response
            }
        });
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            workspace,
            origin,
            config,
            evidence,
            state,
            server,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(local_cli());
        command
            .current_dir(self.workspace.path())
            .env_clear()
            .env("HOME", self.workspace.path().join("home"))
            .env("CODEX_HOME", self.workspace.path().join("home/codex"))
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("NANOCODEX_COMPUTER", "off")
            .args(["--muse", "--muse-oauth-config"])
            .arg(&self.config)
            .arg("--muse-api-base-url")
            .arg(format!("{}/v1", self.origin))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command
    }

    fn run_command(&self, prompt: &str) -> Command {
        let mut command = self.command();
        command.args([
            "run",
            prompt,
            "--browser=none",
            "--mcp-defaults",
            "false",
            "--mcp-codex-config",
            "false",
            "--link-homes",
            "false",
        ]);
        command
    }

    async fn execute(&self, name: &str, mut command: Command) -> Output {
        let output = tokio::time::timeout(Duration::from_secs(40), command.output())
            .await
            .unwrap()
            .unwrap();
        for bytes in [&output.stdout, &output.stderr] {
            let text = String::from_utf8_lossy(bytes);
            for secret in [IDENTITY, KEY_ONE, KEY_TWO, STATIC_KEY, "synthetic-device"] {
                assert!(!text.contains(secret), "credential leaked in {name}");
            }
        }
        std::fs::write(self.evidence.join(format!("{name}.stdout")), &output.stdout).unwrap();
        std::fs::write(self.evidence.join(format!("{name}.stderr")), &output.stderr).unwrap();
        output
    }

    async fn auth(&self, name: &str, action: &str) -> Output {
        let mut command = self.command();
        command.args(["auth", action]);
        if action == "login" {
            command.arg("--no-open");
        }
        self.execute(name, command).await
    }
}
#[cfg(unix)]
struct TuiChild(Box<dyn portable_pty::Child + Send + Sync>);

#[cfg(unix)]
impl Drop for TuiChild {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

#[cfg(unix)]
async fn run_tui(command: Command, artifact: &Path, answer: &str) -> eyre::Result<()> {
    use eyre::eyre;
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    use std::io::{Read, Write};
    let command = command.as_std();
    let mut terminal_command = CommandBuilder::new(command.get_program());
    terminal_command.args(command.get_args());
    terminal_command.env_clear();
    for (key, value) in command.get_envs() {
        if let Some(value) = value {
            terminal_command.env(key, value);
        }
    }
    terminal_command.env("TERM", "xterm-256color");
    if let Some(directory) = command.get_current_dir() {
        terminal_command.cwd(directory);
    }
    std::fs::write(
        artifact.join("scenario.json"),
        serde_json::to_vec_pretty(&json!({
            "reproduce":"cargo test --locked -p nanocodex-bin --test muse_cli -- --nocapture",
            "command":format!("{terminal_command:?}"),
            "expected":format!("TUI displays {answer}, then exits successfully after Ctrl+C Ctrl+C"),
            "boundary":"shipped nanocodex executable in a 140x32 PTY; only external providers use loopback fixtures",
            "input_after_answer":"Ctrl+C Ctrl+C (0x03 0x03)",
            "terminal_transcript":"terminal.log"
        }))?,
    )?;
    let mut transcript = std::fs::File::create(artifact.join("terminal.log"))?;
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 32,
            cols: 140,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|error| eyre!(error.to_string()))?;
    let mut child = TuiChild(
        pair.slave
            .spawn_command(terminal_command)
            .map_err(|error| eyre!(error.to_string()))?,
    );
    drop(pair.slave);
    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|error| eyre!(error.to_string()))?;
    let mut keyboard = pair
        .master
        .take_writer()
        .map_err(|error| eyre!(error.to_string()))?;
    let (send, mut receive) = tokio::sync::mpsc::unbounded_channel();
    let capture = std::thread::spawn(move || -> std::io::Result<()> {
        let mut bytes = [0; 8192];
        while let Ok(count) = reader.read(&mut bytes) {
            if count == 0 {
                break;
            }
            transcript.write_all(&bytes[..count])?;
            let _ = send.send(bytes[..count].to_vec());
        }
        Ok(())
    });
    let mut answer_visible = false;
    let result = tokio::time::timeout(Duration::from_secs(40), async {
        let mut bytes = Vec::new();
        while !String::from_utf8_lossy(&bytes).contains(answer) {
            let chunk = receive
                .recv()
                .await
                .ok_or_else(|| eyre!("TUI closed before displaying {answer}"))?;
            bytes.extend(chunk);
        }
        answer_visible = true;
        // The unified TUI quits on a confirmed Ctrl+C; Ctrl+D edits the composer.
        keyboard.write_all(b"\x03\x03")?;
        keyboard.flush()?;
        loop {
            if let Some(status) = child.0.try_wait()? {
                return Ok::<_, eyre::Report>(status);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .map_err(|_| eyre!("TUI timed out waiting for its answer or clean exit"))
    .and_then(|result| result);
    drop(child);
    drop(keyboard);
    drop(pair.master);
    let capture_result = tokio::task::spawn_blocking(move || capture.join()).await?;
    std::fs::write(
        artifact.join("outcome.json"),
        serde_json::to_vec_pretty(&json!({
            "exit_code":result.as_ref().ok().map(|status| status.exit_code()),
            "success":result.as_ref().is_ok_and(|status| status.success()),
            "answer_visible":answer_visible,
            "error":result.as_ref().err().map(ToString::to_string),
            "terminal_transcript":"terminal.log","provider_transcript":"http.json"
        }))?,
    )?;
    eprintln!("journey evidence: {}", artifact.display());
    capture_result.map_err(|_| {
        eyre!(
            "terminal capture thread panicked; evidence {}",
            artifact.display()
        )
    })??;
    let status = result.map_err(|error| eyre!("{error}; evidence {}", artifact.display()))?;
    if !status.success() {
        return Err(eyre!(
            "TUI failed ({status}); evidence {}",
            artifact.display()
        ));
    }
    Ok(())
}

fn success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn muse_device_login_recovers_across_processes_and_logs_out() {
    let fixture = Fixture::new().await;
    println!("MUSE_CLI_EVIDENCE {}", fixture.evidence.display());
    let login = fixture.auth("login", "login").await;
    success(&login);
    assert!(String::from_utf8_lossy(&login.stderr).contains("TEST-CODE"));
    let home = fixture.workspace.path().join("home/codex");
    let auth_file = home.join("muse/private/auth");
    let saved = std::fs::read(&auth_file).unwrap();
    for secret in [IDENTITY, KEY_ONE] {
        assert!(
            !saved
                .windows(secret.len())
                .any(|bytes| bytes == secret.as_bytes())
        );
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&auth_file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(auth_file.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
    let status = fixture.auth("status", "status").await;
    success(&status);
    assert_eq!(
        serde_json::from_slice::<Value>(&status.stdout).unwrap(),
        json!({"credentials_present":true,"oauth":true})
    );
    let image = fixture.workspace.path().join("input.png");
    std::fs::write(&image,STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==").unwrap()).unwrap();
    let mut command =
        fixture.run_command("[effect] Create the effect and inspect the attached image.");
    command.arg("--image").arg(&image);
    success(&fixture.execute("effect", command).await);
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.path().join("effect.txt")).unwrap(),
        "muse-effect"
    );
    assert_ne!(
        std::fs::read(&auth_file).unwrap(),
        saved,
        "recovered key was not persisted"
    );
    assert_eq!(fixture.state.lock().unwrap().exchanges, 2);
    let sessions = SessionStore::open(&home).unwrap();
    let listed = sessions.list().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].record.family(), HarnessFamily::Muse);
    assert!(
        sessions
            .load(&listed[0].record.session_id)
            .await
            .unwrap()
            .checkpoint
            .is_some()
    );
    #[cfg(unix)]
    {
        let mut command = fixture.command();
        command
            .arg("resume")
            .arg(&listed[0].record.session_id)
            .args([
                "--prompt",
                "[resume] Continue the prior image conversation.",
                "--browser=none",
                "--mcp-defaults",
                "false",
                "--mcp-codex-config",
                "false",
                "--link-homes",
                "false",
            ]);
        run_tui(command, &fixture.evidence, "muse-resumed-ok")
            .await
            .unwrap();
        let state = fixture.state.lock().unwrap();
        let resumed = state
            .requests
            .iter()
            .rev()
            .find_map(|request| request.get("body"))
            .unwrap();
        let history = resumed["input"].to_string();
        assert!(history.contains("[resume]"));
        assert!(
            history.contains("[effect]"),
            "resume discarded prior conversation"
        );
        assert!(
            history.contains("function_call_output"),
            "resume discarded tool results"
        );
    }
    for model in MuseModel::ALL {
        let mut command = fixture.run_command("Continue testing this model.");
        command.args(["--model", model.as_str()]);
        success(&fixture.execute(model.as_str(), command).await);
    }
    success(
        &fixture
            .execute(
                "delegate",
                fixture.run_command("[delegate] Complete work through a Muse child."),
            )
            .await,
    );
    assert_eq!(
        std::fs::read_to_string(fixture.workspace.path().join("child.txt")).unwrap(),
        "muse-child"
    );
    let requests = fixture.state.lock().unwrap().requests.len();
    for (name, flags) in [
        ("wrong-model", vec!["--model", "opus"]),
        ("unsupported-thinking", vec!["--thinking", "none"]),
        ("websocket", vec!["--responses-transport", "websocket"]),
        ("auth-override", vec!["--auth-file", "other.json"]),
    ] {
        let mut command = if name == "auth-override" {
            let mut command = fixture.command();
            command.args(["auth", "status"]);
            command
        } else {
            fixture.run_command("Never dispatch this request.")
        };
        command.args(flags);
        let output = fixture.execute(name, command).await;
        assert!(
            !output.status.success(),
            "invalid selection succeeded: {name}"
        );
        assert_eq!(fixture.state.lock().unwrap().requests.len(), requests);
    }
    {
        let state = fixture.state.lock().unwrap();
        assert_eq!(
            state.exchanges, 2,
            "restart failed to load the recovered inference key"
        );
        assert!(
            state
                .requests
                .iter()
                .filter_map(|request| request.get("body"))
                .any(|body| body.to_string().contains("data:image/png;base64,"))
        );
        for model in MuseModel::ALL {
            assert!(
                state
                    .requests
                    .iter()
                    .any(|request| request["body"]["model"] == model.as_str())
            );
        }
    }
    let public_config = std::fs::read(&fixture.config).unwrap();
    std::fs::write(
        &fixture.config,
        json!({"issuer":"http://untrusted.example", "api_origin":fixture.origin}).to_string(),
    )
    .unwrap();
    let invalid = fixture.auth("invalid-origin", "login").await;
    assert!(!invalid.status.success());
    assert_eq!(fixture.state.lock().unwrap().requests.len(), requests);
    std::fs::write(&fixture.config, public_config).unwrap();
    fixture.state.lock().unwrap().deny_login = true;
    let denied = fixture.auth("denied-login", "login").await;
    assert!(!denied.status.success());
    let after_denial = fixture.auth("status-after-denial", "status").await;
    success(&after_denial);
    assert_eq!(
        serde_json::from_slice::<Value>(&after_denial.stdout).unwrap()["credentials_present"],
        true
    );
    success(&fixture.auth("logout", "logout").await);
    let after_logout = fixture.auth("status-after-logout", "status").await;
    success(&after_logout);
    assert_eq!(
        serde_json::from_slice::<Value>(&after_logout.stdout).unwrap(),
        json!({"credentials_present":false,"oauth":false})
    );
    let requests = fixture.state.lock().unwrap().requests.len();
    let missing = fixture
        .execute("missing-login", fixture.run_command("No model should run."))
        .await;
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("auth login"));
    assert_eq!(fixture.state.lock().unwrap().requests.len(), requests);
    let logged_out = std::fs::read(&auth_file).unwrap();
    let mut command = fixture.run_command("Key-only inference.");
    command.args(["--muse-api-key", STATIC_KEY, "--rollouts", "false"]);
    success(&fixture.execute("key-only", command).await);
    assert_eq!(
        std::fs::read(&auth_file).unwrap(),
        logged_out,
        "explicit API key replaced saved subscription"
    );
    println!(
        "MUSE_CLI device polling, encrypted storage, 401/key exchange, restart, seven model IDs, typed image input, workspace effect, Muse child routing, TUI resume, durability, invalid selection, denied approval, logout and key-only override passed"
    );
}
