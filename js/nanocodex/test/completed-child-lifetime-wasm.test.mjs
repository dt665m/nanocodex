// Run with node --test js/nanocodex/test/completed-child-lifetime-wasm.test.mjs.
// One public SDK / WASM isolate throughout; only remote model responses are synthetic.
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { createServer } from 'node:http';
import { readFile, mkdir, writeFile } from 'node:fs/promises';
import { Agent, Subagents, Transport } from '../host/index.mjs';
import { initializeBrowserEngine } from '../browser/engine.mjs';
import { createMemoryDurabilityStore } from '../runtime/durability-store.mjs';

const expectedOutput = 'completed-' + 'x'.repeat(16_384);
const pause = () => new Promise(resolve => setTimeout(resolve, 5));
test('completed background children release memory across public disposal and recovery in one WASM isolate', { timeout: 120_000 }, async t => {
  let requests = 0;
  const server = createServer(async (request, response) => {
    try {
      const chunks = []; for await (const chunk of request) chunks.push(chunk);
      const body = JSON.parse(Buffer.concat(chunks)); requests++;
      const submitted = body.input.some(item => item.type === 'function_call' && item.name === 'submit_result');
      const definitions = [...(body.tools ?? []), ...body.input.filter(item => item.type === 'additional_tools').flatMap(item => item.tools)];
      const tool = definitions.find(item => item.name === 'submit_result');
      assert.ok(tool);
      response.writeHead(200, { 'content-type': 'text/event-stream' });
      const output = submitted
        ? [{ type: 'message', role: 'assistant', content: [{ type: 'output_text', text: 'COMPLETE' }] }]
        : [{ type: 'function_call', call_id: `result-${requests}`, name: tool.name, arguments: JSON.stringify({ output: expectedOutput }) }];
      response.end(`data: ${JSON.stringify({ type: 'response.completed', response: { id: `fixture-${requests}`, status: 'completed', output, usage: { input_tokens: 1, output_tokens: 1, total_tokens: 2 } } })}\n\n`);
    } catch (error) { response.destroy(error); }
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(async () => { server.closeAllConnections(); await new Promise(resolve => server.close(resolve)); });
  const module = await readFile(new URL('../pkg-web/nanocodex_bg.wasm', import.meta.url));
  const transport = Transport.openAi({ apiKey: 'synthetic-lifetime', apiBaseUrl: `http://127.0.0.1:${server.address().port}/v1`, stateless: true });
  const samples = [], recovered = [];
  const cycles = Number(process.env.LIFETIME_CYCLES ?? 100);
  assert.ok(Number.isSafeInteger(cycles) && cycles >= 40 && cycles % 10 === 0, 'LIFETIME_CYCLES must be a multiple of 10, at least 40');
  for (let index = 0; index < cycles; index++) {
    const durabilityId = `completed-background-${index}`;
    const options = { module, transport, toolMode: 'direct', durability: createMemoryDurabilityStore(durabilityId), durabilityId };
    const agent = await Agent.create(options);
    const sessionId = agent.sessionId;
    let childId;
    try {
      const child = await Subagents.spawn(agent, { role: 'proof', task: 'Submit the required result.', outputSchema: { type: 'string' }, lifetime: 'background' });
      childId = child.agent_id;
      const result = await Subagents.wait(agent, { agentIds: [child.agent_id], timeoutMs: 10_000 });
      assert.equal(result.timed_out, false);
      assert.equal(result.agents[0].status.state, 'completed');
      assert.equal(result.agents[0].status.output, expectedOutput);
    } finally { agent.dispose(); }
    await pause();
    const requestsBeforeRecovery = requests;
    const successor = await Agent.create({ ...options, sessionId });
    try {
      const directory = await Subagents.list(successor, { includeCompleted: true });
      assert.equal(directory.agents.length, 1);
      assert.equal(directory.agents[0].agent_id, childId);
      assert.equal(directory.agents[0].lifetime, 'background');
      const result = await Subagents.wait(successor, { agentIds: [childId], timeoutMs: 10_000 });
      assert.equal(result.timed_out, false);
      assert.deepEqual(result.agents[0].status, { state: 'completed', output: expectedOutput });
      recovered.push({ cycle: index + 1, childId, state: result.agents[0].status.state, outputCharacters: result.agents[0].status.output.length });
    } finally { successor.dispose(); }
    await pause();
    assert.equal(requests, requestsBeforeRecovery, 'completed child recovery must not rerun inference');
    if ((index + 1) % 10 === 0) {
      const sample = { cycles: index + 1, bytes: (await initializeBrowserEngine({ module })).memory.buffer.byteLength };
      samples.push(sample); t.diagnostic(JSON.stringify(sample));
    }
  }
  const evidence = { command: 'node --test js/nanocodex/test/completed-child-lifetime-wasm.test.mjs', cycles, requests, recovered, samples };
  await mkdir(new URL('../../../output/', import.meta.url), { recursive: true });
  await writeFile(new URL('../../../output/wasm-completed-child-memory.json', import.meta.url), JSON.stringify(evidence, null, 2));
  const warmed = samples[2].bytes;
  assert.ok(samples.at(-1).bytes <= warmed + 262_144, `completed background disposal retained growing WASM memory: ${JSON.stringify(samples)}`);
});
