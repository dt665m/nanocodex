//! The shared nanocodex tool contract bridged into Claude's native client
//! tool catalog, with one retained tool runtime per agent.

use std::{path::PathBuf, sync::Arc};

use crate::claude::{
    ClaudeToolInvocation, ClaudeToolReply, ClaudeTools, ToolDefinition, ToolResultContent,
};
use nanocodex_agent::{AgentHandle, NanocodexError, Result};
use nanocodex_oai_tools::{
    ToolContext, ToolDefinition as RuntimeDefinition, ToolInput, Tools,
    contract::{ToolOutputBody, ToolOutputContent},
    runtime::ToolRuntime,
};
use serde_json::{Value, json};

/// Builds this agent's Claude catalog from its materialized tools.
///
/// Code Mode exposes `exec` (JavaScript in `code`) and `wait` over the
/// same Code Mode runtime Codex uses. Direct mode exposes every function tool
/// with its own schema and every freeform tool as a function taking its raw
/// text in `input`; hosted provider tools without a local handler are absent.
pub(super) fn install(
    tools: &Tools,
    code_mode: bool,
    workspace: PathBuf,
    handle: &AgentHandle,
) -> Result<ClaudeTools> {
    let tools = tools.clone().for_session(&handle.session_environment());
    let runtime = Arc::new(ToolRuntime::new_with_tools(workspace, None, None, &tools));
    // Claude awaits adapter cleanup after every admitted turn: cells the turn
    // left running end with it, like a cancelled Codex turn, and the next
    // turn's cells are distinguished. Shell sessions end with the agent.
    let guard = Arc::new(RuntimeGuard(runtime.control()));
    let mut native = ClaudeTools::new().adapter_cleanup(move || {
        let guard = Arc::clone(&guard);
        async move {
            guard.0.cancel_turn().await;
            guard.0.begin_turn();
        }
    });
    for definition in flatten(runtime.model_specs(handle.session_id())) {
        let (name, description, input_schema, call) = match definition {
            RuntimeDefinition::Custom {
                name, description, ..
            } if code_mode && &*name == "exec" => (
                name,
                format!(
                    "{description}\n\nPass the complete JavaScript source as the `code` string."
                ),
                json!({
                    "type": "object",
                    "properties": {"code": {"type": "string", "description": "JavaScript source to evaluate."}},
                    "required": ["code"],
                    "additionalProperties": false,
                }),
                Call::Exec,
            ),
            RuntimeDefinition::Function {
                name,
                description,
                parameters,
                ..
            } if code_mode && &*name == "wait" => (
                name,
                description.into(),
                parameters.as_value().clone(),
                Call::Wait,
            ),
            _ if code_mode => continue,
            RuntimeDefinition::Function { name, .. } | RuntimeDefinition::Custom { name, .. }
                if matches!(&*name, "exec" | "wait") =>
            {
                continue;
            }
            RuntimeDefinition::Function {
                name,
                description,
                parameters,
                strict,
                defer_loading,
                asynchronous,
                ..
            } => {
                direct_supported(&name, defer_loading, asynchronous)?;
                let call = if strict {
                    Call::StrictFunction
                } else {
                    Call::Function
                };
                (
                    name,
                    description.into(),
                    parameters.as_value().clone(),
                    call,
                )
            }
            RuntimeDefinition::Custom {
                name,
                description,
                defer_loading,
                asynchronous,
                format,
            } => (
                {
                    direct_supported(&name, defer_loading, asynchronous)?;
                    name
                },
                format!(
                    "{description}\n\nPass the tool's complete raw text as the `input` string. Input format: {}",
                    serde_json::to_string(&format).unwrap_or_default()
                ),
                json!({
                    "type": "object",
                    "properties": {"input": {"type": "string"}},
                    "required": ["input"],
                    "additionalProperties": false,
                }),
                Call::Freeform,
            ),
            _ => continue,
        };
        let tool_name = name.to_string();
        let definition = ToolDefinition {
            name: tool_name.clone(),
            description,
            input_schema,
            strict: matches!(call, Call::StrictFunction).then_some(true),
            defer_loading: false,
        };
        let runtime = Arc::clone(&runtime);
        native = native.tool_with_context(definition, move |input, invocation| {
            let runtime = Arc::clone(&runtime);
            let name = tool_name.clone();
            async move { call.run(&runtime, &name, input, &invocation).await }
        });
    }
    Ok(native)
}

#[derive(Clone, Copy)]
enum Call {
    Exec,
    Wait,
    Function,
    StrictFunction,
    Freeform,
}

/// Stops shell sessions and cells when the agent's tool catalog is dropped.
struct RuntimeGuard(nanocodex_oai_tools::runtime::ToolRuntimeControl);

impl Drop for RuntimeGuard {
    fn drop(&mut self) {
        let control = self.0.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move { control.cancel().await });
        }
    }
}

/// Claude declares direct client tools eagerly and waits for each result.
fn direct_supported(name: &str, defer_loading: Option<bool>, asynchronous: bool) -> Result<()> {
    if defer_loading == Some(true) || asynchronous {
        return Err(NanocodexError::InvalidRequest(format!(
            "`{name}` is deferred or asynchronous, which Claude direct tools do not support; use Code Mode"
        )));
    }
    Ok(())
}

impl Call {
    async fn run(
        self,
        runtime: &ToolRuntime,
        name: &str,
        input: Value,
        invocation: &ClaudeToolInvocation,
    ) -> std::result::Result<ClaudeToolReply, String> {
        let context = ToolContext::new(
            &invocation.model,
            &invocation.session_id,
            &invocation.call_id,
            &[],
            usize::MAX,
        )
        .with_turn_id(Some(&invocation.turn_id))
        .with_host_context(invocation.host_context.as_deref())
        .with_instruction_revision(invocation.instruction_revision);
        match self {
            Self::Exec => {
                let code = input
                    .get("code")
                    .and_then(Value::as_str)
                    .ok_or("exec requires a `code` string")?;
                let execution = runtime
                    .execute_code(code, context)
                    .await
                    .map_err(|error| error.to_string())?;
                Ok(reply(execution.output, execution.success, None, None))
            }
            Self::Wait => {
                let execution = runtime
                    .wait_for_code(&input.to_string(), context)
                    .await
                    .map_err(|error| error.to_string())?;
                Ok(reply(execution.output, execution.success, None, None))
            }
            Self::Function | Self::StrictFunction | Self::Freeform => {
                let input = match self {
                    Self::Freeform => ToolInput::Freeform(
                        input
                            .get("input")
                            .and_then(Value::as_str)
                            .ok_or("this tool requires an `input` string")?
                            .to_owned(),
                    ),
                    _ => ToolInput::Function(
                        serde_json::value::to_raw_value(&input)
                            .map_err(|error| error.to_string())?,
                    ),
                };
                let output = runtime
                    .execute_tool(name, input, context)
                    .await
                    .map_err(|error| error.to_string())?
                    .into_wire()
                    .map_err(|error| error.to_string())?;
                let decode = |value: Option<Box<serde_json::value::RawValue>>| {
                    value
                        .map(|value| serde_json::from_str(value.get()))
                        .transpose()
                        .map_err(|error| error.to_string())
                };
                Ok(reply(
                    output.output,
                    output.success,
                    decode(output.metadata)?,
                    decode(output.structured_result)?,
                ))
            }
        }
    }
}

/// Restores canonical `namespace__name` dispatch names for grouped tools.
fn flatten(definitions: Vec<RuntimeDefinition>) -> Vec<RuntimeDefinition> {
    let mut flat = Vec::new();
    for definition in definitions {
        match definition {
            RuntimeDefinition::Namespace {
                name: namespace,
                tools,
                ..
            } => {
                for mut tool in tools {
                    match &mut tool {
                        RuntimeDefinition::Function { name, .. }
                        | RuntimeDefinition::Custom { name, .. } => {
                            *name = format!("{namespace}__{name}").into();
                        }
                        _ => continue,
                    }
                    flat.push(tool);
                }
            }
            definition => flat.push(definition),
        }
    }
    flat
}

fn reply(
    output: ToolOutputBody,
    success: bool,
    metadata: Option<Value>,
    structured_result: Option<Value>,
) -> ClaudeToolReply {
    let content = match output {
        ToolOutputBody::Text(text) => ToolResultContent::Text(text),
        ToolOutputBody::Content(items) => {
            ToolResultContent::Blocks(items.into_iter().map(block).collect())
        }
    };
    ClaudeToolReply {
        content,
        is_error: !success,
        metadata,
        structured_result,
    }
}

fn block(item: ToolOutputContent) -> Value {
    match item {
        ToolOutputContent::InputText { text } => json!({"type": "text", "text": text}),
        ToolOutputContent::InputImage { image_url, .. } => {
            match image_url
                .strip_prefix("data:")
                .and_then(|rest| rest.split_once(";base64,"))
            {
                Some((media_type, data)) => json!({
                    "type": "image",
                    "source": {"type": "base64", "media_type": media_type, "data": data},
                }),
                None => json!({"type": "image", "source": {"type": "url", "url": image_url}}),
            }
        }
        other => json!({
            "type": "text",
            "text": serde_json::to_string(&other).unwrap_or_default(),
        }),
    }
}
