// Reproduce after js/nanocodex-vite/scripts/build-js-package.sh:
// node --test js/nanocodex/test/direct-tools-wasm.test.mjs
// Public host and Node SDKs with the generated Rust WASM. Only the external
// Responses/Messages HTTP services are synthetic. Evidence: output/direct-tools-wasm/.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { createServer } from 'node:http';
import { WebSocketServer } from 'ws';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import { Agent, Subagents, Transport, createQuickJsEvaluator } from '../host/index.mjs';
import { Agent as NodeAgent } from '../node/index.mjs';
import asyncVariant from '@jitl/quickjs-wasmfile-release-asyncify';
import { newQuickJSAsyncWASMModuleFromVariant } from 'quickjs-emscripten-core';

const CODE_TOOLS = ['exec', 'wait'];

function messages(blocks, stop = 'end_turn') {
  const frames = [{ type: 'message_start', message: { id: 'fixture', role: 'assistant', model: 'fixture', content: [], usage: { input_tokens: 10, output_tokens: 0 } } }];
  blocks.forEach((content_block, index) => frames.push({ type: 'content_block_start', index, content_block }, { type: 'content_block_stop', index }));
  frames.push({ type: 'message_delta', delta: { stop_reason: stop }, usage: { output_tokens: 1 } }, { type: 'message_stop' });
  return frames.map(frame => `event: ${frame.type}\ndata: ${JSON.stringify(frame)}\n\n`).join('');
}

test('toolMode direct exposes host tools natively for both families while code-only stays the default', { timeout: 120_000 }, async () => {
  const module = await WebAssembly.compile(await readFile(new URL('../pkg-web/nanocodex_bg.wasm', import.meta.url)));
  const trace = [], effects = [], fixtureErrors = [], observed = {};
  let responseId = 0;
  // WebSocket continuations send only new items after previous_response_id.
  const responses = new Map();
  const respond = async (request, response) => {
    try {
      const chunks = []; for await (const chunk of request) chunks.push(chunk);
      const body = JSON.parse(Buffer.concat(chunks));
      const claude = request.url === '/v1/messages';
      assert.equal(request.url, claude ? '/v1/messages' : '/v1/responses');
      const previous = body.previous_response_id === undefined ? undefined : responses.get(body.previous_response_id);
      if (body.previous_response_id !== undefined) assert.ok(previous, 'continuations name a fixture response');
      const history = claude ? body.messages : [...(previous?.history ?? []), ...body.input];
      const encoded = JSON.stringify(history.filter(item => item.type !== 'additional_tools'));
      const definitions = claude ? (body.tools ?? []) : [...(body.tools ?? []), ...history.filter(item => item.type === 'additional_tools').flatMap(item => item.tools)];
      const names = definitions.map(tool => tool.name).sort();
      const marker = encoded.match(/(DIRECT|DEFAULT)_[A-Z_]+_CASE/)?.[0] ?? 'UNMARKED';
      trace.push({ path: request.url, marker, tools: names, body });
      (observed[marker] ??= []).push(names);
      const reply = (calls, text) => {
        response.writeHead(200, { 'content-type': 'text/event-stream' });
        if (claude) {
          response.end(messages(calls.length ? calls.map(([id, name, input]) => ({ type: 'tool_use', id, name, input }))
            : [{ type: 'text', text }], calls.length ? 'tool_use' : 'end_turn'));
        } else {
          const output = calls.length ? calls.map(([call_id, name, input]) => ({ type: 'function_call', call_id, name, arguments: JSON.stringify(input) }))
            : [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text }] }];
          const id = `fixture-${++responseId}`;
          responses.set(id, { history: [...history, ...output] });
          response.end(`data: ${JSON.stringify({ type: 'response.completed', response: { id, status: 'completed', output, usage: { input_tokens: 10, output_tokens: 1, total_tokens: 11 } } })}\n\n`);
        }
      };
      if (marker.startsWith('DEFAULT_')) {
        // Code Mode remains the default: only exec and wait reach the provider.
        assert.deepEqual(names, CODE_TOOLS, marker);
        return reply([], 'DEFAULT_DONE');
      }
      assert.ok(names.includes('probe'), `${marker} exposes the host tool directly: ${names}`);
      assert.ok(!names.includes('exec') && !names.includes('wait'), `${marker} never exposes Code Mode controls: ${names}`);
      const probe = definitions.find(tool => tool.name === 'probe');
      assert.deepEqual(claude ? probe.input_schema : probe.parameters, probeSchema, 'the host schema reaches the provider unchanged');
      const id = marker.toLowerCase();
      const child = marker.includes('_CHILD_');
      if (!encoded.includes(`${id}-probe`)) return reply([[`${id}-probe`, 'probe', { label: marker }]]);
      assert.match(encoded, new RegExp(`PROBE_RECEIPT:${marker}`), 'the real handler result returns to the model');
      if (child) {
        assert.ok(names.includes('submit_result'), `direct children receive native subagent tools: ${names}`);
        if (!encoded.includes(`${id}-submit`)) return reply([[`${id}-submit`, 'submit_result', { output: { ok: true, marker } }]]);
      }
      return reply([], `DONE:${marker}`);
    } catch (error) { fixtureErrors.push(String(error?.stack ?? error)); response.destroy(error); }
  };
  const server = createServer(respond);
  // The Node SDK streams Responses over WebSocket; serve it from the same fixture.
  const sockets = new WebSocketServer({ server });
  sockets.on('connection', socket => socket.on('message', bytes => respond({
    url: '/v1/responses', async *[Symbol.asyncIterator]() { yield bytes; },
  }, { writeHead() {}, end(frame) { socket.send(frame.slice('data: '.length).trim()); }, destroy(error) { fixtureErrors.push(String(error)); socket.terminate(); } })));
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const base = `http://127.0.0.1:${server.address().port}`;
  const probeSchema = { type: 'object', properties: { label: { type: 'string' } }, required: ['label'], additionalProperties: false };
  const handler = (input, context) => {
    effects.push({ label: input.label, model: context.model, sessionId: context.sessionId, subagent: context.subagent?.agentId ?? null });
    return `PROBE_RECEIPT:${input.label}`;
  };
  // Every Codex transport targets this fixture: hosts use stateless HTTP and the
  // Node SDK uses the fixture WebSocket, so no request can reach a provider.
  const codex = (extra = {}) => ({ module, model: 'gpt-6.1-sol', thinking: 'low',
    tools: [{ name: 'probe', description: 'Record one synthetic effect', parameters: probeSchema, handler }],
    transport: Transport.openAi({ apiKey: 'synthetic-codex', apiBaseUrl: `${base}/v1`, stateless: true }), ...extra });
  const claude = (extra = {}) => ({ module, harness: 'claude', model: 'claude-sonnet-5-5', thinking: 'low',
    endpoint: `${base}/v1/messages`, auth: { apiKey: 'synthetic-claude' },
    tools: [{ name: 'probe', description: 'Record one synthetic effect', inputSchema: probeSchema, handler }], ...extra });
  const roots = [];
  const journeys = [];
  let failure;
  try {
    // 1. Direct roots: each SDK and family exposes probe itself; the model calls it once.
    const nodeTransport = Transport.openAi({ apiKey: 'synthetic-codex', websocketUrl: `ws://127.0.0.1:${server.address().port}`, websocketWarmup: false });
    for (const [sdkName, SDK, extra] of [['HOST', Agent, {}], ['NODE', NodeAgent, { module: undefined }]]) {
      for (const [family, options] of [['CODEX', codex], ['CLAUDE', claude]]) {
        const marker = `DIRECT_${sdkName}_${family}_CASE`;
        const before = effects.length;
        const root = await SDK.create(options({ ...extra, ...(sdkName === 'NODE' && family === 'CODEX' ? { transport: nodeTransport } : {}), toolMode: 'direct' }));
        roots.push(root);
        const result = await root.turn.prompt({ input: marker }).result();
        assert.equal(result.finalMessage, `DONE:${marker}`);
        assert.deepEqual(effects.slice(before).map(effect => effect.label), [marker], 'the real handler ran exactly once');
        journeys.push({ marker, finalMessage: result.finalMessage, providerTools: observed[marker] });
      }
    }
    // 2. Direct roots with the shared task tree: a child spawned through the
    //    public Subagents API calls probe and submit_result as native tools.
    //    CROSS: a direct Claude root runs a direct alternate Codex recipe
    //    without any Code Mode evaluator.
    for (const [family, options, extra, selection] of [
      ['CODEX', codex, {}, {}],
      ['CLAUDE', claude, { subagents: {} }, {}],
      ['CROSS', claude, { subagents: {}, harnesses: { codex: codex({ toolMode: 'direct' }) } }, { harness: 'codex', model: 'sol' }],
    ]) {
      const marker = `DIRECT_CHILD_${family}_CASE`;
      const root = await Agent.create(options({ toolMode: 'direct', ...extra }));
      roots.push(root);
      const spawned = await Subagents.spawn(root, { role: 'direct fixture', ...selection, task: `${marker}: call probe once, then submit the typed result.`,
        outputSchema: { type: 'object', properties: { ok: { type: 'boolean' }, marker: { type: 'string' } }, required: ['ok', 'marker'], additionalProperties: false } });
      const report = await Subagents.wait(root, { agentIds: [spawned.agent_id], timeoutMs: 20_000 });
      assert.equal(report.timed_out, false, JSON.stringify(report));
      assert.equal(report.agents[0].status.state, 'completed', JSON.stringify(report));
      assert.deepEqual(report.agents[0].status.output, { ok: true, marker });
      const childEffects = effects.filter(effect => effect.label === marker);
      assert.equal(childEffects.length, 1, 'the child handler ran exactly once');
      assert.ok(childEffects[0].subagent, 'the host handler receives canonical child identity');
      await Subagents.close(root, spawned.agent_id);
      journeys.push({ marker, output: report.agents[0].status.output, providerTools: observed[marker], childAgentId: childEffects[0].subagent });
    }
    // 3. Code-only remains the default for both families.
    const codeEvaluator = createQuickJsEvaluator(await newQuickJSAsyncWASMModuleFromVariant(asyncVariant));
    for (const [family, options] of [['CODEX', codex], ['CLAUDE', claude]]) {
      const marker = `DEFAULT_${family}_CASE`;
      const root = await Agent.create(options({ codeEvaluator }));
      roots.push(root);
      assert.equal((await root.turn.prompt({ input: marker }).result()).finalMessage, 'DEFAULT_DONE');
      assert.deepEqual(observed[marker], [CODE_TOOLS]);
      journeys.push({ marker, providerTools: observed[marker] });
    }
    // 4. Direct mode keeps remote MCP behind Code Mode and fails before dispatch.
    const beforeRejected = trace.length;
    await assert.rejects(Agent.create(codex({ toolMode: 'direct', mcp: { fixture: 'https://mcp.invalid/mcp' } })), /remote MCP requires Code Mode/);
    await assert.rejects(Agent.create(claude({ toolMode: 'direct', serverTools: [{ type: 'web_search_20250305', name: 'web_search' }] })), /provider or client search tools/);
    assert.equal(trace.length, beforeRejected, 'unsupported direct configurations never reach a provider');
    assert.equal(effects.find(effect => effect.label === 'DIRECT_CHILD_CROSS_CASE').model, 'gpt-6.1-sol', 'the cross-family child runs the selected Codex model');
    assert.equal(effects.length, 7, 'four direct roots and three direct children each committed one effect');
    assert.deepEqual(fixtureErrors, []);
  } catch (error) { failure = String(error?.stack ?? error); throw error; } finally {
    // Bound teardown so a failed journey reports promptly instead of hanging CI.
    let timer;
    const teardown = await Promise.race([
      Promise.allSettled(roots.map(root => root.session.shutdown())).then(() => 'settled'),
      new Promise(resolve => { timer = setTimeout(() => resolve('timed out'), 10_000); }),
    ]);
    clearTimeout(timer);
    for (const socket of sockets.clients) socket.terminate();
    await new Promise(resolve => sockets.close(resolve));
    server.closeAllConnections(); await new Promise(resolve => server.close(resolve));
    const output = new URL('../../../output/direct-tools-wasm/', import.meta.url);
    await mkdir(output, { recursive: true });
    await writeFile(new URL('trace.json', output), JSON.stringify({
      command: 'node --test js/nanocodex/test/direct-tools-wasm.test.mjs',
      expected: 'direct roots (host+Node SDK, Codex+Claude) expose probe without exec/wait and run it once; direct children (Codex, Claude, and a Codex recipe under a Claude root) spawned via Subagents.spawn call probe and submit_result natively; default roots expose only exec/wait; direct+MCP and Claude provider tools reject before dispatch',
      observed: { journeys, effects, providerTools: observed, teardown }, failure: failure ?? null, fixtureErrors, trace,
    }, null, 2));
  }
});
