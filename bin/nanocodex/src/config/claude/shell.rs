//! Retained Bash jobs. Each job owns a workspace runtime so stopping one cannot
//! cancel another. The existing foreground executor supplies capture/deadlines.
use super::*;
use std::collections::BTreeMap;
use tokio::sync::{Mutex, watch};

struct Job {
    runtime: Arc<WorkspaceToolRuntime>,
    worker: Option<tokio::task::JoinHandle<()>>,
    result: watch::Receiver<Option<std::result::Result<String, String>>>,
    stopped: bool,
}
impl Drop for Job {
    fn drop(&mut self) {
        if let Some(worker) = &self.worker {
            worker.abort();
        }
        let runtime = self.runtime.clone();
        tokio::spawn(async move { runtime.control().cancel().await });
    }
}

pub(super) struct Shell {
    workspace: Arc<worktree::Workspace>,
    cwd: Mutex<(PathBuf, PathBuf)>,
    jobs: Mutex<BTreeMap<String, Job>>,
}
impl Shell {
    pub(super) fn new(workspace: Arc<worktree::Workspace>) -> Self {
        let current = workspace.current();
        Self {
            workspace,
            cwd: Mutex::new((current.clone(), current)),
            jobs: Mutex::new(BTreeMap::new()),
        }
    }
    pub(super) fn definition() -> ToolDefinition {
        let mut schema = ClaudeBash::<RetainedBash>::definitions().remove(0);
        schema["description"] = json!(
            "Run Bash in the authorized workspace. Foreground working-directory changes inside the project carry to the next Bash call; outside-project directories reset to the project root. Background jobs snapshot the current directory without changing it. Environment exports do not carry. Background commands return a task_id for TaskOutput and TaskStop. Timeout includes process cleanup; output is bounded. No sandbox bypass."
        );
        schema["input_schema"]["properties"]["run_in_background"]["description"] = json!(
            "Run as a retained background task; use TaskOutput to read its result and TaskStop to stop it."
        );
        serde_json::from_value(schema).expect("Bash definition")
    }
    pub(super) async fn execute(
        &self,
        mut input: Value,
    ) -> std::result::Result<ClaudeToolReply, String> {
        let background = input
            .get("run_in_background")
            .map_or(Some(false), Value::as_bool)
            .ok_or("invalid run_in_background")?;
        // Validate synchronously before admitting a background task. This uses
        // the same adapter parser with an executor that performs no effects.
        struct Validate;
        impl SandboxBashExecutor for Validate {
            async fn execute(&self, _: BashRequest) -> std::result::Result<BashResult, String> {
                Ok(BashResult {
                    stdout: String::new(),
                    stderr: String::new(),
                    exit_code: 0,
                    truncated: false,
                })
            }
        }
        if let Some(object) = input.as_object_mut() {
            object.insert("run_in_background".into(), json!(false));
        }
        ClaudeBash::new(Validate)
            .execute("Bash", input.clone())
            .await?;
        // Serialize foreground turns so their observed cwd is applied in order.
        // A background job snapshots cwd but never changes the next command's cwd.
        let (workspace, workspace_lease) = self.workspace.pin_current();
        let mut cwd = self.cwd.lock().await;
        if cwd.0 != workspace {
            *cwd = (workspace.clone(), workspace.clone());
        }
        let start = cwd
            .1
            .canonicalize()
            .ok()
            .filter(|p| p.starts_with(&workspace) && p.is_dir())
            .unwrap_or_else(|| workspace.clone());
        let runtime = Arc::new(WorkspaceToolRuntime::new(start));
        let retained = RetainedBash {
            runtime: runtime.clone(),
            gate: Arc::new(Mutex::new(())),
        };
        if !background {
            let receipt = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
            let shell = ClaudeBash::new(CwdBash {
                retained,
                receipt: receipt.path().into(),
            });
            let output = shell.execute("Bash", input).await;
            // EXIT traps observe the shell's real final directory, including
            // compound commands and early `exit`. Missing/invalid receipts reset.
            let observed = receipt
                .as_file()
                .metadata()
                .ok()
                .filter(|m| m.len() <= 4096)
                .and_then(|_| std::fs::read_to_string(receipt.path()).ok())
                .and_then(|p| {
                    Path::new(p.strip_suffix('\n').unwrap_or(&p))
                        .canonicalize()
                        .ok()
                })
                .filter(|p| p.starts_with(&workspace) && p.is_dir());
            cwd.1 = observed.unwrap_or_else(|| workspace.clone());
            return output.map(text_reply);
        }
        drop(cwd);
        let shell = ClaudeBash::new(retained);
        let mut jobs = self.jobs.lock().await;
        if jobs.len() >= 256 {
            return Err("Bash task limit reached (256 per session)".into());
        }
        let id = format!("bash-{}", uuid::Uuid::new_v4());
        let (sender, result) = watch::channel(None);
        let worker = tokio::spawn(async move {
            let _workspace_lease = workspace_lease;
            sender.send_replace(Some(shell.execute("Bash", input).await));
        });
        jobs.insert(
            id.clone(),
            Job {
                runtime,
                worker: Some(worker),
                result,
                stopped: false,
            },
        );
        Ok(text_reply(
            json!({"task_id":id,"status":"running"}).to_string(),
        ))
    }
    pub(super) async fn output(
        &self,
        id: &str,
        block: bool,
        timeout: u64,
    ) -> std::result::Result<ClaudeToolReply, String> {
        let (mut result, stopped) = {
            let jobs = self.jobs.lock().await;
            let job = jobs.get(id).ok_or("unknown Bash task_id in this session")?;
            (job.result.clone(), job.stopped)
        };
        if block && !stopped && result.borrow().is_none() && timeout > 0 {
            let _ = tokio::time::timeout(Duration::from_millis(timeout), result.changed()).await;
        }
        let outcome = result.borrow().clone();
        // A stop may have arrived while this poll was waiting.
        let stopped = self
            .jobs
            .lock()
            .await
            .get(id)
            .is_some_and(|job| job.stopped);
        let reply = match outcome {
            Some(Ok(output)) => {
                json!({"task_id":id,"status":"completed","output":serde_json::from_str::<Value>(&output).unwrap_or(json!(output))})
            }
            Some(Err(error)) => json!({"task_id":id,"status":"failed","error":error}),
            None => json!({"task_id":id,"status":if stopped {"stopped"} else {"running"}}),
        };
        Ok(text_reply(reply.to_string()))
    }
    pub(super) async fn stop(&self, id: &str) -> std::result::Result<ClaudeToolReply, String> {
        let mut jobs = self.jobs.lock().await;
        let job = jobs
            .get_mut(id)
            .ok_or("unknown Bash task_id in this session")?;
        if job.result.borrow().is_some() {
            return Ok(text_reply(
                json!({"task_id":id,"status":"already_finished"}).to_string(),
            ));
        }
        if let Some(worker) = job.worker.take() {
            worker.abort();
            let _ = worker.await;
        }
        job.runtime.control().cancel().await;
        job.stopped = true;
        Ok(text_reply(
            json!({"task_id":id,"status":"stopped"}).to_string(),
        ))
    }
}

// The receipt is private host bookkeeping, kept separate from bounded stdout.
struct CwdBash {
    retained: RetainedBash,
    receipt: PathBuf,
}
impl SandboxBashExecutor for CwdBash {
    async fn execute(&self, mut request: BashRequest) -> std::result::Result<BashResult, String> {
        fn quote(value: &str) -> String {
            format!("'{}'", value.replace('\'', "'\"'\"'"))
        }
        let trap = format!(
            "command pwd -P > {}",
            quote(&self.receipt.to_string_lossy())
        );
        request.command = format!("trap {} EXIT\n{}", quote(&trap), request.command);
        self.retained.execute(request).await
    }
}
