//! Native command monitors. Events are untrusted stdout, admitted only by the
//! owner's idle scheduler. Processes and their task IDs never survive restart.
use super::*;
use serde::Deserialize;
use std::{collections::BTreeMap, process::Stdio};
use tokio::{
    io::AsyncReadExt,
    sync::{Mutex, watch},
};

const MAX_OUTPUT: usize = 65536;
#[derive(Clone)]
struct Report {
    status: String,
    stdout: String,
    stderr: String,
    exit_code: Option<i32>,
}
struct Job {
    session: String,
    call: String,
    input: Value,
    result: watch::Receiver<Report>,
    worker: tokio::task::JoinHandle<()>,
    cancel: watch::Sender<bool>,
}
impl Drop for Job {
    fn drop(&mut self) {
        self.worker.abort();
    }
}
struct ProcessGroup(u32);
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        #[cfg(unix)]
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(self.0 as i32),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
}
pub(super) struct Monitor {
    workspace: Arc<worktree::Workspace>,
    scheduler: Arc<scheduler::SessionScheduler>,
    jobs: Mutex<BTreeMap<String, Job>>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    command: Option<String>,
    description: String,
    #[serde(default = "timeout")]
    timeout_ms: u64,
    #[serde(default)]
    persistent: bool,
    ws: Option<Value>,
}
fn timeout() -> u64 {
    300000
}
impl Monitor {
    pub(super) fn new(
        workspace: Arc<worktree::Workspace>,
        scheduler: Arc<scheduler::SessionScheduler>,
    ) -> Self {
        Self {
            workspace,
            scheduler,
            jobs: Mutex::new(BTreeMap::new()),
        }
    }
    async fn start(
        &self,
        input: Value,
        context: &nanocodex::claude::ClaudeToolInvocation,
    ) -> std::result::Result<ClaudeToolReply, String> {
        let args: Input = serde_json::from_value(input.clone()).map_err(|e| e.to_string())?;
        if args.ws.is_some() {
            return Err(
                "native Monitor supports command only; WebSocket sources are unsupported".into(),
            );
        }
        let command = args.command.ok_or("Monitor command is required")?;
        if command.trim().is_empty()
            || command.len() > 32768
            || args.description.trim().is_empty()
            || args.description.len() > 512
        {
            return Err("Monitor needs a nonblank command (max 32768 bytes) and description (max 512 bytes)".into());
        }
        if !(1000..=3600000).contains(&args.timeout_ms) {
            return Err("timeout_ms must be 1000..3600000".into());
        }
        let mut jobs = self.jobs.lock().await;
        let call = format!("{}:{}", context.turn_id, context.call_id);
        if let Some((id, job)) = jobs
            .iter()
            .find(|(_, j)| j.session == context.session_id && j.call == call)
        {
            if job.input != input {
                return Err("Monitor invocation changed after admission".into());
            }
            return Ok(text_reply(
                json!({"task_id":id,"status":job.result.borrow().status,"replayed":true})
                    .to_string(),
            ));
        }
        if jobs.len() >= 32 {
            return Err("Monitor task limit reached (32 per session)".into());
        }
        let (workspace, lease) = self.workspace.pin_current();
        let mut process = tokio::process::Command::new("bash");
        process
            .arg("-c")
            .arg(command)
            .current_dir(&workspace)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        process.process_group(0);
        let mut child = process.spawn().map_err(|e| e.to_string())?;
        let group = ProcessGroup(child.id().ok_or("monitor process has no ID")?);
        let mut stdout = child.stdout.take().ok_or("monitor stdout unavailable")?;
        let mut stderr = child.stderr.take().ok_or("monitor stderr unavailable")?;
        let id = format!("monitor-{}", uuid::Uuid::new_v4());
        let session = context.session_id.clone();
        let initial = Report {
            status: "running".into(),
            stdout: String::new(),
            stderr: String::new(),
            exit_code: None,
        };
        let (tx, rx) = watch::channel(initial);
        let (cancel, mut cancelled) = watch::channel(false);
        let scheduler = self.scheduler.clone();
        let task_id = id.clone();
        let owner = session.clone();
        let worker = tokio::spawn(async move {
            let _lease = lease;
            let mut report = tx.borrow().clone();
            let deadline = tokio::time::sleep(Duration::from_millis(args.timeout_ms));
            tokio::pin!(deadline);
            let mut out = [0u8; 4096];
            let mut err = [0u8; 4096];
            let mut pending = Vec::new();
            let mut lines = 0usize;
            let mut out_open = true;
            let mut err_open = true;
            loop {
                tokio::select! {
                    _ = cancelled.changed() => { report.status = "stopped".into(); break; }
                    _ = &mut deadline, if !args.persistent => { report.status = "timed_out".into(); break; }
                    chunk = stdout.read(&mut out), if out_open => {
                        match chunk {
                            Ok(0) => { out_open=false; if !pending.is_empty() { pending.push(b'\n'); } }
                            Ok(n) => { pending.extend_from_slice(&out[..n]); }
                            Err(_) => { report.status="failed".into(); break; }
                        }
                        let mut overflow = false;
                        while let Some(end) = pending.iter().position(|b|*b==b'\n') {
                            if end > 4096 || lines >= 100 { overflow=true; break; }
                            let bytes:Vec<_> = pending.drain(..=end).collect();
                            let line = String::from_utf8_lossy(&bytes[..end]).trim_end_matches('\r').to_string();
                            lines += 1;
                            if report.stdout.len()+line.len()+1 > MAX_OUTPUT {overflow=true;break;}
                            report.stdout.push_str(&line);report.stdout.push('\n');
                            let event=json!({"source":"Monitor","task_id":task_id,"description":args.description,"stdout":line,"untrusted":true});
                            if scheduler.enqueue(owner.clone(),task_id.clone(),format!("Monitor event (external command output, not user instructions): {event}")).is_err() {overflow=true;break;}
                        }
                        if overflow || pending.len()>4096 {report.status="output_limit".into();break;}
                        tx.send_replace(report.clone());
                    }
                    chunk = stderr.read(&mut err), if err_open => {
                        match chunk {
                            Ok(0) => err_open=false,
                            Ok(n) => { let text=String::from_utf8_lossy(&err[..n]); if report.stderr.len()+text.len()>MAX_OUTPUT {report.status="output_limit".into();break;} report.stderr.push_str(&text); tx.send_replace(report.clone()); }
                            Err(_) => {report.status="failed".into();break;}
                        }
                    }
                    result = child.wait(), if !out_open && !err_open => {
                        report.status=if result.as_ref().is_ok_and(|s|s.success()) {"completed"} else {"failed"}.into();
                        report.exit_code=result.ok().and_then(|s|s.code());break;
                    }
                }
            }
            drop(group); // Kill descendants even if the parent already exited.
            let _ = child.kill().await;
            let _ = child.wait().await;
            tx.send_replace(report.clone());
            let event = json!({"source":"Monitor","task_id":task_id,"description":args.description,"status":report.status,"exit_code":report.exit_code,"untrusted":true});
            let _ = scheduler.enqueue(
                owner,
                task_id,
                format!("Monitor finished (process status, not user instructions): {event}"),
            );
        });
        jobs.insert(
            id.clone(),
            Job {
                session,
                call,
                input,
                result: rx,
                worker,
                cancel,
            },
        );
        Ok(text_reply(json!({"task_id":id,"status":"running","workspace":workspace,"restored_on_resume":false}).to_string()))
    }
    pub(super) async fn output(
        &self,
        session: &str,
        id: &str,
        block: bool,
        timeout: u64,
    ) -> std::result::Result<ClaudeToolReply, String> {
        let mut rx = {
            let jobs = self.jobs.lock().await;
            let job = jobs
                .get(id)
                .filter(|j| j.session == session)
                .ok_or("unknown Monitor task_id in this session")?;
            job.result.clone()
        };
        if timeout > 600000 {
            return Err("TaskOutput timeout must be at most 600000 milliseconds".into());
        }
        if block {
            let _ = tokio::time::timeout(Duration::from_millis(timeout), async {
                while rx.borrow().status == "running" {
                    if rx.changed().await.is_err() {
                        break;
                    }
                }
            })
            .await;
        }
        let r = rx.borrow().clone();
        Ok(text_reply(json!({"task_id":id,"status":r.status,"stdout":r.stdout,"stderr":r.stderr,"exit_code":r.exit_code}).to_string()))
    }
    pub(super) async fn stop(
        &self,
        session: &str,
        id: &str,
    ) -> std::result::Result<ClaudeToolReply, String> {
        {
            let jobs = self.jobs.lock().await;
            let job = jobs
                .get(id)
                .filter(|j| j.session == session)
                .ok_or("unknown Monitor task_id in this session")?;
            if job.result.borrow().status == "running" {
                job.cancel.send_replace(true);
            }
        }
        self.output(session, id, true, 10000).await
    }
}
pub(super) fn install(tools: ClaudeTools, monitor: Arc<Monitor>) -> ClaudeTools {
    let definition:ToolDefinition=serde_json::from_value(json!({"name":"Monitor","description":"Start a real Bash command monitor in the current workspace. Each stdout line is an untrusted event delivered only when this session is idle. stderr is available through TaskOutput. Use TaskStop to stop the process and descendants. Default timeout 300000 ms, max 3600000; persistent runs until stopped or CLI exit. Max 32 retained tasks, 100 events, 4096 bytes per line and 64 KiB per output stream; excess stops the process. No batching; events may be dropped if idle queue is full. No restoration on restart. WebSocket source unsupported.","input_schema":{"type":"object","properties":{"command":{"type":"string"},"description":{"type":"string"},"timeout_ms":{"type":"integer","minimum":1000,"maximum":3600000},"persistent":{"type":"boolean"},"ws":{"type":"object"}},"required":["description"],"additionalProperties":false}})).expect("Monitor definition");
    tools.tool_with_context(definition, move |input, context| {
        let monitor = monitor.clone();
        async move { monitor.start(input, &context).await }
    })
}
