//! Claude-native task-tree surface backed by the same host registry as Codex.
use super::*;
use nanocodex::agent::AgentHandle;
use nanocodex::claude::ClaudeToolInvocation;
use nanocodex_subagents::{AgentTask, Registry, start_fork_agent};
use serde::Deserialize;

fn definition(
    name: &str,
    description: &str,
    properties: Value,
    required: &[&str],
) -> ToolDefinition {
    serde_json::from_value(json!({"name":name,"description":description,"input_schema":{"type":"object","properties":properties,"required":required,"additionalProperties":false}})).expect("native host definition")
}

pub(super) fn install(
    mut native: ClaudeTools,
    runtime: Arc<RetainedHost>,
    shell: Arc<shell::Shell>,
    enabled: bool,
    fork: Option<(AgentHandle, Arc<Registry>)>,
    monitor: Option<Arc<monitor::Monitor>>,
) -> ClaudeTools {
    let mut definitions = vec![
        definition(
            "TaskOutput",
            "Read a retained Bash or Monitor task or agent result. A nonblocking poll never stops the task. Task IDs are scoped to this session/task tree and do not survive process restart.",
            json!({"task_id":{"type":"string"},"block":{"type":"boolean","default":true},"timeout":{"type":"integer","minimum":0,"maximum":600000,"default":30000}}),
            &["task_id"],
        ),
        definition(
            "TaskStop",
            "Stop a retained Bash or Monitor process (including descendants) or interrupt an authorized child agent. Completed output remains available.",
            json!({"task_id":{"type":"string"}}),
            &["task_id"],
        ),
    ];
    if enabled {
        definitions.extend([
            definition("Agent", "Start a clean-room child using the shared agent registry. Supply its complete prompt. Foreground waits up to five minutes and returns a task_id if still running; background returns immediately. Use subagent_type=fork to inherit the native conversation through the boundary preceding this tool batch. Forks always run in the background on the same model (model override ignored); harness, thinking, resume and output_contract overrides are rejected. Otherwise general-purpose starts a fresh conversation. Optional harness/model and output_contract allow cross-family delegation. Resume sends a new delegated prompt to an owned child, preserving its result contract.", json!({"prompt":{"type":"string"},"description":{"type":"string"},"subagent_type":{"type":"string","enum":["general-purpose","fork"],"default":"general-purpose"},"model":{"type":["string","null"]},"harness":{"type":["string","null"],"enum":["claude","codex","xai",null]},"thinking":{"type":["string","null"]},"output_contract":{"type":"object"},"resume":{"type":"string"},"run_in_background":{"type":"boolean","default":false}}), &["prompt","description"]),
            definition("CloseAgent", "Close an owned child and its descendants, releasing their retained sessions. Closed children cannot be resumed.", json!({"task_id":{"type":"string"}}), &["task_id"]),
            definition("ListAgents", "List agents in this task tree with their real lifecycle state and management permissions.", json!({"include_completed":{"type":"boolean"},"include_self":{"type":"boolean"}}), &[]),
            definition("SendMessage", "Send a bounded message to an agent in the same task tree. recipient is its numeric ID or agent-N task ID. Deferred delivery may queue; finish the current turn so queued messages can run. Delegation requires management authority.", json!({"recipient":{"type":"string"},"content":{"type":"string","maxLength":2048},"priority":{"type":"string","enum":["deferred","urgent"]},"purpose":{"type":"string","enum":["delegate","coordinate","finding","question","reply"]},"in_reply_to":{"type":"integer"}}), &["recipient","content"]),
            definition("SubmitResult", "Submit the current child agent's structured result. This is the native entry for the submit_result operation named in shared registry instructions. After an accepted receipt, finish with a brief final message. Root agents return their answer normally.", json!({"output":{}}), &["output"]),
        ]);
    }
    for definition in definitions {
        let name = definition.name.clone();
        let runtime = runtime.clone();
        let shell = shell.clone();
        let fork = fork.clone();
        let monitor = monitor.clone();
        native = native.tool_with_context(definition, move |input, invocation| {
            let runtime = runtime.clone();
            let shell = shell.clone();
            let name = name.clone();
            let fork = fork.clone();
            let monitor = monitor.clone();
            async move {
                execute(
                    &runtime,
                    &shell,
                    &name,
                    input,
                    &invocation,
                    fork.as_ref(),
                    monitor.as_deref(),
                )
                .await
            }
        });
    }
    native
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentInput {
    prompt: String,
    description: String,
    #[serde(default)]
    subagent_type: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    harness: Option<String>,
    #[serde(default)]
    thinking: Option<String>,
    #[serde(default)]
    output_contract: Option<Value>,
    #[serde(default)]
    resume: Option<String>,
    #[serde(default)]
    run_in_background: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OutputInput {
    task_id: String,
    #[serde(default = "yes")]
    block: bool,
    #[serde(default = "wait_ms")]
    timeout: u64,
}
fn yes() -> bool {
    true
}
fn wait_ms() -> u64 {
    30_000
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StopInput {
    task_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MessageInput {
    recipient: String,
    content: String,
    #[serde(default)]
    priority: Option<String>,
    #[serde(default)]
    purpose: Option<String>,
    #[serde(default)]
    in_reply_to: Option<u64>,
}

fn agent_id(id: &str) -> std::result::Result<u64, String> {
    id.strip_prefix("agent-")
        .unwrap_or(id)
        .parse::<u64>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| "expected an agent-N task_id or positive agent ID".into())
}
async fn call(
    runtime: &RetainedHost,
    name: &str,
    input: Value,
    invocation: &ClaudeToolInvocation,
) -> std::result::Result<ClaudeToolReply, String> {
    let context = ToolContext::new(
        &invocation.model,
        &invocation.session_id,
        &invocation.call_id,
        &[],
        16000,
    )
    .with_turn_id(Some(&invocation.turn_id))
    .with_host_context(invocation.host_context.as_deref())
    .with_instruction_revision(invocation.instruction_revision);
    let output = runtime
        .execute_tool(
            name,
            ToolInput::Function(to_raw_value(&input).map_err(|e| e.to_string())?),
            context,
        )
        .await
        .map_err(|e| e.to_string())?;
    let mut reply = runtime_reply(&output.output, output.success)?;
    reply.structured_result = Some(output.structured_result());
    reply.metadata = output
        .metadata
        .as_ref()
        .and_then(|value| serde_json::from_str(value.get()).ok());
    Ok(reply)
}
async fn wait(
    runtime: &RetainedHost,
    id: u64,
    block: bool,
    timeout: u64,
    invocation: &ClaudeToolInvocation,
) -> std::result::Result<ClaudeToolReply, String> {
    let reply = call(
        runtime,
        "wait_agent",
        json!({"agent_ids":[id],"timeout_ms":if block {timeout.clamp(1,300000)} else {1}}),
        invocation,
    )
    .await?;
    if reply.is_error {
        return Ok(reply);
    }
    let report = reply.structured_result.unwrap_or(Value::Null);
    Ok(text_reply(
        json!({"task_id":format!("agent-{id}"),"report":report}).to_string(),
    ))
}
async fn execute(
    runtime: &RetainedHost,
    shell: &shell::Shell,
    name: &str,
    input: Value,
    invocation: &ClaudeToolInvocation,
    fork: Option<&(AgentHandle, Arc<Registry>)>,
    monitor: Option<&monitor::Monitor>,
) -> std::result::Result<ClaudeToolReply, String> {
    match name {
        "Agent" => {
            let args: AgentInput = serde_json::from_value(input).map_err(|e| e.to_string())?;
            if args.prompt.trim().is_empty() || args.description.trim().is_empty() {
                return Err("Agent prompt and description must be nonblank".into());
            }
            if args.subagent_type.as_deref() == Some("fork") {
                if args.harness.is_some()
                    || args.thinking.is_some()
                    || args.resume.is_some()
                    || args.output_contract.is_some()
                {
                    return Err("fork preserves its native model/thinking and starts a new child; harness, thinking, resume and output_contract overrides are unsupported".into());
                }
                let (parent, registry) = fork.ok_or("native fork registry is unavailable")?;
                let report = start_fork_agent(
                    parent,
                    registry,
                    &invocation.session_id,
                    AgentTask {
                        role: args.description,
                        task: format!(
                            "{}\n\nUse SubmitResult to submit your string result.",
                            args.prompt
                        ),
                        output_schema: json!({"type":"string"}),
                    },
                )
                .await
                .map_err(|error| error.to_string())?;
                return Ok(text_reply(json!({"task_id":format!("agent-{}", report.agent_id),"agent_id":report.agent_id,"status":"running"}).to_string()));
            }
            if args
                .subagent_type
                .as_deref()
                .is_some_and(|kind| kind != "general-purpose")
            {
                return Err("only general-purpose subagent_type is configured".into());
            }
            let id = if let Some(resume) = args.resume {
                if args.model.is_some()
                    || args.harness.is_some()
                    || args.thinking.is_some()
                    || args.output_contract.is_some()
                {
                    return Err("resume preserves the existing harness, model, thinking and output contract".into());
                }
                let id = agent_id(&resume)?;
                let reply = call(runtime, "send_agent_message", json!({"agent_id":id,"message":args.prompt,"purpose":"delegate","priority":"deferred"}), invocation).await?;
                if reply.is_error {
                    return Ok(reply);
                }
                // A queued delegation cannot be synchronously waited on: the
                // current owner must finish its turn to release it.
                return Ok(text_reply(json!({"task_id":format!("agent-{id}"),"delivery":reply.structured_result,"next":"Use TaskOutput on a later turn."}).to_string()));
            } else {
                let model = match args.model.as_deref() {
                    Some("sonnet") => Some("claude-sonnet-5-5"),
                    Some("opus") => Some("claude-opus-5-5"),
                    Some("fable") => Some("claude-fable-5-1"),
                    Some("haiku") => Some("claude-haiku-4-5"),
                    other => other,
                };
                let thinking = if model == Some("claude-haiku-4-5") && args.thinking.is_none() {
                    Some("none")
                } else {
                    args.thinking.as_deref()
                };
                let reply = call(runtime, "spawn_agent", json!({"role":args.description,"task":format!("{}\n\nIf your catalog exposes SubmitResult, use it for the shared submit_result operation.", args.prompt),"harness":args.harness,"model":model,"thinking":thinking,"output_contract":args.output_contract.unwrap_or(json!({"kind":"string"}))}), invocation).await?;
                if reply.is_error {
                    return Ok(reply);
                }
                reply
                    .structured_result
                    .as_ref()
                    .and_then(|value| value["agent_id"].as_u64())
                    .ok_or("agent registry returned no agent_id")?
            };
            if args.run_in_background {
                Ok(text_reply(
                    json!({"task_id":format!("agent-{id}"),"agent_id":id,"status":"running"})
                        .to_string(),
                ))
            } else {
                wait(runtime, id, true, 300000, invocation).await
            }
        }
        "TaskOutput" => {
            let args: OutputInput = serde_json::from_value(input).map_err(|e| e.to_string())?;
            if args.timeout > 600000 {
                return Err("TaskOutput timeout must be at most 600000 milliseconds".into());
            }
            if args.task_id.starts_with("monitor-") {
                monitor
                    .ok_or("Monitor is unavailable in this session")?
                    .output(
                        &invocation.session_id,
                        &args.task_id,
                        args.block,
                        args.timeout,
                    )
                    .await
            } else if args.task_id.starts_with("bash-") {
                shell.output(&args.task_id, args.block, args.timeout).await
            } else {
                wait(
                    runtime,
                    agent_id(&args.task_id)?,
                    args.block,
                    args.timeout,
                    invocation,
                )
                .await
            }
        }
        "TaskStop" => {
            let args: StopInput = serde_json::from_value(input).map_err(|e| e.to_string())?;
            if args.task_id.starts_with("monitor-") {
                monitor
                    .ok_or("Monitor is unavailable in this session")?
                    .stop(&invocation.session_id, &args.task_id)
                    .await
            } else if args.task_id.starts_with("bash-") {
                shell.stop(&args.task_id).await
            } else {
                call(
                    runtime,
                    "interrupt_agent",
                    json!({"agent_id":agent_id(&args.task_id)?}),
                    invocation,
                )
                .await
            }
        }
        "CloseAgent" => {
            let args: StopInput = serde_json::from_value(input).map_err(|e| e.to_string())?;
            call(
                runtime,
                "close_agent",
                json!({"agent_id":agent_id(&args.task_id)?}),
                invocation,
            )
            .await
        }
        "ListAgents" => call(runtime, "list_agents", input, invocation).await,
        "SubmitResult" => call(runtime, "submit_result", input, invocation).await,
        "SendMessage" => {
            let args: MessageInput = serde_json::from_value(input).map_err(|e| e.to_string())?;
            let mut input = json!({"agent_id":agent_id(&args.recipient)?,"message":args.content,"priority":args.priority.unwrap_or("deferred".into()),"purpose":args.purpose.unwrap_or("coordinate".into())});
            if let Some(reply_to) = args.in_reply_to {
                input["in_reply_to"] = json!(reply_to);
            }
            call(runtime, "send_agent_message", input, invocation).await
        }
        _ => Err(format!("unknown native host tool: {name}")),
    }
}
