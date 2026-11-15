// Reproduce: node --test js/nanocodex/test/nanocodex-runtime-wasm.test.mjs
// Public Nanocodex.create runtime over the generated Rust WASM (Node placement).
// Only the external Responses/Messages providers are synthetic; credentials are
// synthetic values. Evidence: output/nanocodex-runtime-wasm/trace.json.
import assert from "node:assert/strict";
import { test } from "node:test";
import { createServer } from "node:http";
import { mkdir, writeFile } from "node:fs/promises";
import { WebSocketServer } from "ws";

import {
  ChatGptSubscription,
  Nanocodex,
  Subagents,
  Transport,
  createMemoryChatGptSubscriptionStore,
} from "../node/index.mjs";
import { Nanocodex as HostNanocodex, Transport as HostTransport } from "../host/index.mjs";
import { Nanocodex as BrowserNanocodex } from "../browser/index.mjs";

const CHILD_SCHEMA = { type: "object", properties: { ok: { type: "boolean" }, model: { type: "string" } }, required: ["ok", "model"], additionalProperties: false };

function messages(blocks, stop = "end_turn") {
  const frames = [{ type: "message_start", message: { id: "fixture", role: "assistant", model: "fixture", content: [], usage: { input_tokens: 10, output_tokens: 0 } } }];
  blocks.forEach((content_block, index) => frames.push({ type: "content_block_start", index, content_block }, { type: "content_block_stop", index }));
  frames.push({ type: "message_delta", delta: { stop_reason: stop }, usage: { output_tokens: 1 } }, { type: "message_stop" });
  return frames.map((frame) => "event: " + frame.type + "\ndata: " + JSON.stringify(frame) + "\n\n").join("");
}

function jwt(exp) {
  const encode = (value) => Buffer.from(JSON.stringify(value)).toString("base64url");
  return encode({ alg: "none" }) + "." + encode({ exp }) + ".";
}

test("Nanocodex.create configures backends once and owns independent roots", { timeout: 120_000 }, async () => {
  const trace = [], effects = [], fixtureErrors = [], observations = {};
  let responseId = 0;
  const chains = new Map();
  const accessToken = jwt(Math.floor(Date.now() / 1000) + 3600);

  // One synthetic provider: Claude Messages over HTTP, Codex Responses over WebSocket.
  const respond = (family, body, auth) => {
    const history = family === "claude" ? body.messages : body.input;
    // A chained Responses request carries only new items; replay its predecessor's history.
    const encoded = (family === "codex" && body.previous_response_id ? chains.get(body.previous_response_id) ?? "" : "") + JSON.stringify(history ?? []);
    const definitions = family === "claude" ? body.tools ?? [] : [...(body.tools ?? []), ...(body.input ?? []).filter((item) => item.type === "additional_tools").flatMap((item) => item.tools)];
    const row = { family, model: body.model, auth, tools: definitions.map((tool) => tool.name).sort(),
      instructionMarkers: ["RUNTIME_INSTRUCTIONS", "CLAUDE_ROOT_INSTRUCTIONS"].filter((marker) => JSON.stringify(family === "claude" ? body.system ?? "" : [body.instructions ?? "", (body.input ?? []).filter((item) => item.role === "developer" || item.role === "system")]).includes(marker)),
      bodyKeys: Object.keys(body),
      thinking: family === "claude" ? body.thinking ?? body.output_config ?? null : body.reasoning ?? null,
      sawFirstTurn: encoded.includes("FIRST_TURN"), previousResponseId: body.previous_response_id ?? null };
    trace.push(row);
    const child = encoded.includes("CHILD_TASK");
    const submitted = encoded.includes("tools.submit_result(");
    const proved = encoded.includes("RUNTIME_EFFECT_RECEIPT");
    const tool = !child ? (encoded.includes("CALL_PROOF") && !proved ? "proof" : undefined)
      : !proved ? "proof" : !submitted ? "submit_result" : undefined;
    const args = tool === "submit_result" ? { output: { ok: true, model: body.model } } : {};
    const code = tool ? "text(await tools." + tool + "(" + JSON.stringify(args) + "));" : undefined;
    const text = "DONE " + body.model;
    if (family === "claude") {
      return messages(tool ? [{ type: "tool_use", id: "code-" + ++responseId, name: "exec", input: { code } }] : [{ type: "text", text }], tool ? "tool_use" : "end_turn");
    }
    const output = tool ? [{ type: "custom_tool_call", call_id: "code-" + ++responseId, name: "exec", input: code }]
      : [{ type: "message", role: "assistant", content: [{ type: "output_text", text }] }];
    const id = "response-" + ++responseId;
    chains.set(id, encoded + JSON.stringify(output));
    return JSON.stringify({ type: "response.completed", response: { id, status: "completed", output, usage: { input_tokens: 10, output_tokens: 1, total_tokens: 11 } } });
  };
  const server = createServer(async (request, response) => {
    try {
      const chunks = []; for await (const chunk of request) chunks.push(chunk);
      assert.equal(request.url, "/v1/messages");
      const body = JSON.parse(Buffer.concat(chunks));
      response.writeHead(200, { "content-type": "text/event-stream" });
      response.end(respond("claude", body, request.headers["x-api-key"] ? "claude-api-key:" + request.headers["x-api-key"] : "none"));
    } catch (error) { fixtureErrors.push(String(error)); response.destroy(error); }
  });
  const sockets = new WebSocketServer({ server });
  sockets.on("connection", (socket, request) => {
    const authorization = request.headers.authorization ?? "";
    const auth = authorization === "Bearer " + accessToken && request.headers["chatgpt-account-id"] === "account-synthetic"
      ? "chatgpt-subscription" : authorization === "Bearer synthetic-openai" ? "openai-api-key" : "unexpected:" + authorization.slice(0, 16);
    socket.on("message", (bytes) => {
      try {
        const body = JSON.parse(bytes.toString("utf8"));
        if (body.type !== "response.create") return;
        socket.send(respond("codex", body, auth));
      } catch (error) { fixtureErrors.push(String(error)); socket.terminate(); }
    });
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const port = server.address().port;
  const endpoint = "http://127.0.0.1:" + port + "/v1/messages";
  const websocketUrl = "ws://127.0.0.1:" + port;

  const subscriptionStore = createMemoryChatGptSubscriptionStore("runtime-journey");
  const subscription = await ChatGptSubscription.open({ id: "runtime-journey", store: subscriptionStore,
    seed: { accessToken, refreshToken: "synthetic-refresh", accountId: "account-synthetic" } });
  const factoryCalls = [];
  const proofTool = (harness) => ({ name: "proof", description: "Commit one synthetic effect",
    [harness === "claude" ? "inputSchema" : "parameters"]: { type: "object", properties: {}, additionalProperties: false },
    handler(_input, context) {
      effects.push({ harness, model: context.model, sessionId: context.sessionId, child: Boolean(context.subagent) });
      return "RUNTIME_EFFECT_RECEIPT";
    } });
  const backends = {
    codex: { transport: Transport.chatGpt({ subscription, websocketUrl, websocketWarmup: false }),
      tools: ({ harness, role }) => { factoryCalls.push({ harness, role }); return [proofTool("codex")]; } },
    claude: { transport: Transport.anthropic({ apiKey: "synthetic-claude", endpoint }), maxTokens: 1024,
      tools: ({ harness, role }) => { factoryCalls.push({ harness, role }); return [proofTool("claude")]; } },
  };
  const runtimes = [];
  try {
    // Configuration errors fail before any provider dispatch.
    const rejections = [];
    const rejects = async (label, operation, pattern) => {
      const before = trace.length;
      await assert.rejects(operation, pattern, label);
      assert.equal(trace.length, before, label + " dispatched nothing");
      rejections.push(label);
    };
    await rejects("no backends", () => Nanocodex.create({ backends: {}, model: "gpt-6.1-sol" }), /at least one backend/);
    await rejects("runtime-level tools", () => Nanocodex.create({ backends, tools: [] }), /tools belong to each backend/);
    await rejects("runtime-level sessionId", () => Nanocodex.create({ backends, sessionId: "fixed" }), /session identity is per Agent/);
    await rejects("model of the other family", () => Nanocodex.create({ backends, harness: "codex", model: "claude-opus-5-5" }), /belongs to the claude harness/);
    await rejects("unknown model", () => Nanocodex.create({ backends: { codex: backends.codex }, model: "gpt-unknown" }), /unknown model/);
    await rejects("unsupported thinking", () => Nanocodex.create({ backends, model: "claude-haiku-4-5", thinking: "xhigh" }), /does not accept thinking/);
    await rejects("missing default backend", () => Nanocodex.create({ backends: { codex: backends.codex }, harness: "claude" }), /no claude backend/);
    await rejects("managed transport", () => Nanocodex.create({ backends: { codex: Transport.managed({ apiKey: "synthetic", agent: { create: true } }) } }), /Transport.managed/);
    await rejects("ChatGPT subscription as Claude credential", async () => Transport.anthropic({ subscription }), /open ClaudeSubscription/);
    await rejects("createTools instance", () => Nanocodex.create({ backends: { codex: { ...backends.codex, tools: { attach() {}, close() {} } } } }), /createTools\(\) instance/);

    const nano = await Nanocodex.create({ backends, instructions: "RUNTIME_INSTRUCTIONS", workspace: "/workspace/runtime" });
    runtimes.push(nano);
    observations.defaults = nano.defaults;
    assert.deepEqual(nano.defaults, { harness: "codex", model: "gpt-6.1-sol", thinking: nano.defaults.thinking }, "codex is the default family when configured; its catalog default model");
    assert.deepEqual([...nano.backends], ["codex", "claude"]);

    const onlyCodex = await Nanocodex.create({ backends: { codex: backends.codex }, model: "gpt-6.1-sol", thinking: "low" });
    runtimes.push(onlyCodex);
    await rejects("family without a backend", () => onlyCodex.spawn({ harness: "claude" }), /no claude backend is configured/);
    await rejects("spawn model of a missing family", () => onlyCodex.spawn({ model: "claude-sonnet-5-5" }), /no claude backend/);
    await rejects("spawn harness/model mismatch", () => nano.spawn({ harness: "codex", model: "claude-sonnet-5-5" }), /belongs to the claude harness/);
    await rejects("spawn unsupported option", () => nano.spawn({ sessionId: "x" }), /unsupported nano.spawn option/);

    // Independent roots, each with its own tool factory result and session.
    const prompt = async (agent, input) => {
      const turn = agent.turn.prompt({ input });
      const result = await turn.result();
      const message = result.finalMessage;
      result.dispose(); turn.dispose();
      return message;
    };
    const codexRoot = await nano.spawn();
    const claudeRoot = await nano.spawn({ harness: "claude", instructions: "CLAUDE_ROOT_INSTRUCTIONS" });
    const sonnetRoot = await nano.spawn({ model: "claude-sonnet-5-5" });
    const thinkingRoot = await nano.spawn({ thinking: "high" });
    const roots = { codexRoot, claudeRoot, sonnetRoot, thinkingRoot };
    observations.roots = Object.fromEntries(Object.entries(roots).map(([name, agent]) => [name, agent.session.info()]));
    assert.equal(new Set(Object.values(roots).map((agent) => agent.sessionId)).size, 4, "every spawn is an independent root session");
    for (const agent of Object.values(roots)) assert.equal(agent.session.info().lineage.origin, "root");
    assert.equal(claudeRoot.session.info().harness, "claude");
    assert.equal(codexRoot.session.info().harness, "codex");

    assert.equal(await prompt(codexRoot, "CALL_PROOF codex root"), "DONE gpt-6.1-sol");
    assert.equal(await prompt(claudeRoot, "CALL_PROOF claude root"), "DONE claude-opus-5-5", "family switch uses the destination family default model");
    assert.equal(await prompt(sonnetRoot, "plain sonnet"), "DONE claude-sonnet-5-5", "model selection infers its family");
    assert.equal(await prompt(thinkingRoot, "plain thinking"), "DONE gpt-6.1-sol");
    const byModel = (model) => trace.filter((row) => row.model === model);
    assert.ok(byModel("gpt-6.1-sol").every((row) => row.auth === "chatgpt-subscription"), "Codex uses only the ChatGPT subscription");
    assert.ok(trace.filter((row) => row.family === "claude").every((row) => row.auth === "claude-api-key:synthetic-claude"), "Claude uses only its API key");
    // Incremental WebSocket continuations omit the unchanged tool list.
    assert.ok(trace.every((row) => !row.tools.length || row.tools.join() === "exec,wait"), "Code Mode exposes only exec/wait");
    assert.ok(trace.some((row) => row.family === "codex" && row.instructionMarkers.includes("RUNTIME_INSTRUCTIONS")), "runtime instructions apply");
    assert.ok(trace.some((row) => row.family === "claude" && row.instructionMarkers.includes("CLAUDE_ROOT_INSTRUCTIONS")), "spawn instructions override");
    const rootEffects = effects.filter((effect) => !effect.child);
    assert.deepEqual(rootEffects.map((effect) => effect.harness), ["codex", "claude"]);
    assert.notEqual(rootEffects[0].sessionId, rootEffects[1].sessionId, "tool calls run in each root's own session");
    assert.ok(factoryCalls.filter((call) => call.role === "root").length >= 4, "tool factory materializes per root");
    observations.thinking = { sonnet: sonnetRoot.session.info(), trace: trace.map((row) => ({ model: row.model, thinking: row.thinking })) };

    // Children route to the configured family with that family's credential.
    for (const [root, harness, model] of [[codexRoot, "claude", "claude-opus-5-5"], [claudeRoot, "codex", "gpt-6.1-sol"]]) {
      const spawned = await Subagents.spawn(root, { role: "fixture child", task: "CHILD_TASK: call proof once, then submit the typed result.", harness, outputSchema: CHILD_SCHEMA });
      const report = await Subagents.wait(root, { agentIds: [spawned.agent_id], timeoutMs: 20_000 });
      assert.equal(report.timed_out, false, JSON.stringify(report));
      assert.equal(report.agents[0].status.state, "completed", JSON.stringify(report));
      assert.equal(report.agents[0].status.output.model, model, harness + " child uses its family default");
      await Subagents.close(root, spawned.agent_id);
    }
    const childEffects = effects.filter((effect) => effect.child);
    assert.deepEqual(childEffects.map((effect) => effect.harness), ["claude", "codex"], "children execute their own family's tools");
    assert.ok(trace.filter((row) => row.family === "codex").every((row) => row.auth === "chatgpt-subscription"), "the Claude root's Codex child uses the ChatGPT subscription");

    // codeMode: false exposes each backend's host tools directly, without exec/wait.
    const direct = await Nanocodex.create({ backends: { codex: { ...backends.codex, codeMode: false }, claude: { ...backends.claude, codeMode: false } } });
    runtimes.push(direct);
    observations.direct = {};
    for (const harness of ["codex", "claude"]) {
      const agent = await direct.spawn({ harness });
      const before = trace.length;
      assert.match(await prompt(agent, "plain direct " + harness), /^DONE /);
      observations.direct[harness] = trace[before].tools;
      assert.ok(trace[before].tools.includes("proof") && !trace[before].tools.includes("exec"), harness + " direct mode exposes host tools natively");
    }
    await direct.shutdown();

    // Checkpoint resume with the runtime's current credentials.
    for (const [agent, family] of [[claudeRoot, "claude"], [codexRoot, "codex"]]) {
      assert.match(await prompt(agent, "FIRST_TURN " + family), /^DONE /);
      const checkpoint = JSON.parse(JSON.stringify(await agent.session.checkpoint()));
      const sessionId = agent.sessionId;
      await agent.session.shutdown();
      await rejects("resume with the wrong harness", () => nano.resume(checkpoint, { harness: family === "claude" ? "codex" : "claude" }), /belongs to the/);
      const resumed = await nano.resume(checkpoint);
      assert.equal(resumed.sessionId, sessionId, "resume keeps the session identity");
      assert.equal(resumed.session.info().harness, family);
      const before = trace.length;
      assert.match(await prompt(resumed, "after resume"), /^DONE /);
      const row = trace[before];
      assert.ok(row.sawFirstTurn || row.previousResponseId, family + " resumed conversation continues its history");
      observations["resume_" + family] = { sessionId, row };
      roots[family === "claude" ? "claudeRoot" : "codexRoot"] = resumed;
    }

    // Runtime ownership: forks and disposed handles are joined by shutdown.
    const fork = await roots.codexRoot.session.fork();
    assert.equal(fork.session.info().lineage.origin, "fork");
    roots.sonnetRoot.dispose();
    assert.throws(() => roots.sonnetRoot.session.info(), /disposed/);
    const late = nano.spawn({ model: "claude-haiku-5-5" });
    await nano.shutdown();
    await assert.rejects(late.then((agent) => agent.session.info()), /shut down|disposed/, "an admission racing shutdown is rejected or shut down");
    for (const agent of [fork, roots.codexRoot, roots.claudeRoot, roots.thinkingRoot]) {
      assert.throws(() => agent.turn.prompt({ input: "after shutdown" }), /disposed/, "shutdown joined every owned agent");
    }
    await assert.rejects(nano.spawn(), /shut down/);
    await assert.rejects(nano.resume(await Promise.resolve({ format: "nanocodex-session-checkpoint/1", model: "gpt-6.1-sol" })), /shut down/);
    assert.equal(await nano.shutdown(), undefined, "shutdown is idempotent");

    // Web API placements expose the same factory over the bundled WASM catalog.
    const { readFile } = await import("node:fs/promises");
    const module = await WebAssembly.compile(await readFile(new URL("../pkg-web/nanocodex_bg.wasm", import.meta.url)));
    await assert.rejects(HostNanocodex.create({ backends: { claude: HostTransport.anthropic({ apiKey: "synthetic", endpoint }) }, module }), /explicit codeEvaluator/);
    for (const factory of [HostNanocodex, BrowserNanocodex]) {
      const runtime = await factory.create({ backends: { claude: HostTransport.anthropic({ apiKey: "synthetic", endpoint }) }, module, codeEvaluator: async () => {} });
      assert.equal(runtime.defaults.model, "claude-opus-5-5");
      await runtime.shutdown();
    }
    observations.rejections = rejections;
    assert.deepEqual(fixtureErrors, []);
  } finally {
    await Promise.allSettled(runtimes.map((runtime) => runtime.shutdown()));
    subscription.dispose();
    for (const socket of sockets.clients) socket.terminate();
    await new Promise((resolve) => sockets.close(resolve));
    server.closeAllConnections(); await new Promise((resolve) => server.close(resolve));
    const output = new URL("../../../output/nanocodex-runtime-wasm/", import.meta.url);
    await mkdir(output, { recursive: true });
    await writeFile(new URL("trace.json", output), JSON.stringify({
      command: "node --test js/nanocodex/test/nanocodex-runtime-wasm.test.mjs",
      expected: "pre-dispatch configuration errors; codex default family and catalog default model; independent roots with per-root tools; family/model/thinking selection; cross-family children on their own credentials; checkpoint resume keeps identity and history; shutdown joins forks, disposed handles and racing admissions",
      observations, factoryCalls, effects, fixtureErrors,
      trace: trace.map(({ family, model, auth, tools, thinking, instructionMarkers, sawFirstTurn, previousResponseId }) => ({ family, model, auth, tools, thinking, instructionMarkers, sawFirstTurn, previousResponseId })),
    }, null, 2));
  }
});
