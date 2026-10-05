# Native Claude CLI command hooks

Pass `--claude --claude-hooks /absolute/path/settings.json` to explicitly enable
synchronous local command hooks. The CLI does not discover command hooks from
repository or home settings. The selected configuration also applies to native
Claude children. Commands execute with the current user's local permissions;
choose a settings file you trust.

```json
{
  "hooks": {
    "PreToolUse": [{
      "matcher": "^(Bash|Write)$",
      "hooks": [{"type": "command", "command": "/absolute/path/check-tool", "timeout": 10}]
    }],
    "PostToolUse": [{
      "matcher": "Bash",
      "hooks": [{"type": "command", "command": "/absolute/path/record-result"}]
    }],
    "PostToolUseFailure": [{
      "hooks": [{"type": "command", "command": "/absolute/path/record-failure"}]
    }]
  }
}
```

Matchers use Rust regular expressions against the native tool name; empty or
`*` matches every tool. Matching hooks run sequentially in configuration order.
Unsupported events, non-command hook types, invalid matchers and invalid timeouts
reject the configuration before model inference.

Each command receives one JSON object on stdin followed by a newline. Fields are
`hook_event_name`, `session_id`, `turn_id`, `tool_use_id`, `tool_name`, `tool_input`,
`cwd`, `model`, and nullable `instruction_revision`. Post events add
`tool_response` and `is_error`; failure events also include `error`. The invocation
identity is stable across that tool's pre/post hooks. Hooks wrap native client
tools; provider-side server tools cannot be intercepted.

A successful command may produce empty stdout or a JSON object. Pre hooks can
return `{"continue":false,"stopReason":"reason"}` or
`{"decision":"block","reason":"reason"}` to deny execution. The native structured
form supports `hookSpecificOutput` with `hookEventName:"PreToolUse"`,
`permissionDecision` (`allow`, `deny`, or `ask`),
`permissionDecisionReason`, and object-valued `updatedInput`. `ask` blocks because
this host has no hook approval dialog. Updated inputs pass to subsequent hooks
and normal tool validation; hooks do not grant new capabilities.

Nonzero exits, malformed decision fields, malformed JSON, output overflow and
timeouts fail a pre hook and prevent execution. Exit 2 is reported as a blocking
hook exit, including bounded stderr. A post hook failure or blocking decision
appends an error to the original tool receipt, retaining its result and reminding
the caller that completed effects have not been undone. Failure hooks observe
handler failures, not denials or failures that prevent the handler from running.
Plan mode denies blocked model tool calls before configured hooks run. Allowed
inspection, context/skill loading, task-board and interaction calls still run their
configured hooks, including after a planning session is restored. Trusted hook
commands can have effects of their own. Plan mode is a dispatch policy, not OS
isolation or rollback of earlier effects.

Additional context injection, asynchronous hooks, prompt/agent hooks, interactive
hook approval, transcript paths and other Claude Code lifecycle events are not
implemented. Successful output fields outside the documented subset are ignored.

Permission rules are checked before hooks and against the final rewritten input.
An `allow` hook decision cannot override a deny rule; a policy approval prompt
concerns the exact final call. Hook-requested `ask` remains unsupported.

Commands run through `/bin/sh -c` without a login shell, with the invoking session’s current workspace cwd,
`CLAUDE_PROJECT_DIR`, and `PATH=/usr/bin:/bin`. Other inherited environment variables
are cleared. Use absolute executable paths where needed. Settings and serialized
stdin are limited to 1 MiB each (stdin has one additional newline); stdout and
stderr are independently limited to 64 KiB. Timeout defaults to 60 seconds and
must be greater than zero and at most 600 seconds. The limit covers stdin writes,
process exit and pipe draining. Unix process groups are killed on completion,
failure, timeout or cancellation, including descendants holding pipes open.
This is process cleanup, not an isolation boundary for hostile hook executables.

Hooks run inside the admitted durable tool effect. Replaying a committed receipt
or completed request does not rerun its hooks. A crash before receipt commit
still requires reconciling hook side effects using invocation identity; arbitrary
shell effects are not exactly-once transactions.

Run the shipped CLI acceptance journey with:

```sh
CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_INCREMENTAL=0 \
  cargo +1.97.0 test -p nanocodex-bin --test claude_hooks -- --nocapture
```

The journey uses only a synthetic Messages provider; hook commands, tool effects,
HTTP/SSE and SQLite are real. Inspect commands, stdin logs, provider requests,
stdout/stderr and outcomes under `output/claude-hooks-cli/`.
