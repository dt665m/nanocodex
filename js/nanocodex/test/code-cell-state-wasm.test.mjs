import assert from 'node:assert/strict';
import { mkdtemp, mkdir, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { Worker } from 'node:worker_threads';
import test from 'node:test';
import { createManagedCodeEffectJournal } from './support/managed-code-journal.mjs';
import { createSqliteDurabilityStore, sqliteDurabilitySchema } from '../runtime/durability-store.mjs';
import { startResponsesServer, messageReader, sendCompleted, sendFinal } from './support/responses.mjs';

// Real public Node SDK, compiled Rust WASM, asyncify QuickJS, and the shipped
// journal. Adapt only DurableObjectStorage to a persistent local SQLite file
// and the external model to a deterministic Responses WebSocket provider.
for (const kind of ['success', 'failure', 'abort']) test(`WASM cold cell ${kind}: atomic state and terminal receipt survive owner loss`, { timeout: 60000 }, async t => {
  const directory = await mkdtemp(join(tmpdir(), 'code-cell-wasm-'));
  const database = new DatabaseSync(join(directory, 'state.sqlite'));
  for (const sql of sqliteDurabilitySchema) database.exec(sql);
  const query = (sql, args = []) => database.prepare(sql).all(...args);
  function transaction(fn) {
    database.exec('BEGIN IMMEDIATE');
    try { const result = fn(); database.exec('COMMIT'); return result; }
    catch (error) { database.exec('ROLLBACK'); throw error; }
  }
  const storage = {
    sql: { exec(sql, ...args) {
      if (args.length === 0 && sql.includes(';')) { database.exec(sql); return { toArray: () => [] }; }
      const rows = query(sql, args);
      return { toArray: () => rows, one: () => { assert.equal(rows.length, 1); return rows[0]; }, [Symbol.iterator]: () => rows[Symbol.iterator]() };
    } }, transactionSync: transaction, async sync() {},
  };
  const durability = createSqliteDurabilityStore({ transaction: fn => transaction(() => fn(query)) });
  const server = await startResponsesServer();
  const trace = [], messages = [], waiters = [], workers = [];
  let generation = 0, journal, acknowledgementLost = false;
  const sessionId = '018f1f9a-7b3c-7a07-8000-000000000021';
  const dispatches = [];
  const until = predicate => new Promise(resolve => {
    const check = () => { const message = messages.find(predicate); if (message) { waiters.splice(waiters.indexOf(check), 1); resolve(message); } };
    waiters.push(check); check();
  });
  function start() {
    generation++; const owner = generation;
    journal = createManagedCodeEffectJournal(storage);
    const ownedJournal = journal;
    const worker = new Worker(new URL('./support/code-recovery-owned.worker.mjs', import.meta.url), {
      workerData: { sdk: 'node', journal: true, cellJournal: true, evaluator: 'quickjs', url: server.url },
    });
    workers.push(worker);
    worker.on('error', error => { messages.push({ owner, type: 'worker-error', error: error.message }); for (const check of [...waiters]) check(); });
    worker.on('message', async message => {
      trace.push({ owner, ...message });
      if (message.type !== 'rpc') { messages.push({ owner, ...message }); for (const check of [...waiters]) check(); return; }
      try {
        assert.equal(owner, generation, 'previous owner fenced');
        let result;
        if (message.method.startsWith('durability.')) result = await durability[message.method.slice(11)](...message.args);
        else if (message.method === 'effect') {
          dispatches.push(message.args[0]);
          messages.push({ owner, type: 'dispatch', kind: message.args[0] }); for (const check of [...waiters]) check();
          if (message.args[0] === 'poison') return;
          result = null;
        } else {
          result = await ownedJournal[message.method.slice(8)](...message.args);
          if (message.method === 'journal.completeCell' && kind !== 'abort' && !acknowledgementLost) {
            acknowledgementLost = true;
            throw new Error('fixture lost ACK after atomic SQLite cell commit');
          }
        }
        worker.postMessage({ id: message.id, result });
      } catch (error) { worker.postMessage({ id: message.id, error: error.message }); }
    });
    return { worker, owner };
  }
  t.after(async () => {
    for (const worker of workers) await worker.terminate();
    await server.close();
    const output = join(process.cwd(), 'output', 'code-cell-wasm'); await mkdir(output, { recursive: true });
    await writeFile(join(output, kind + '.json'), JSON.stringify({ command: 'node --experimental-transform-types --import ./js/managed/test-fixtures/node-types-loader.mjs --test js/nanocodex/test/code-cell-state-wasm.test.mjs', kind, dispatches, trace }, null, 2));
    database.close(); await rm(directory, { recursive: true, force: true });
  });
  let { worker, owner } = start();
  let socket = await server.nextConnection(), reader = messageReader(socket);
  await reader.next();
  const source = kind === 'success'
    ? 'store("counter", (load("counter") ?? 0) + 1); text(await tools.effect({kind:"read-one"})); text({counter:load("counter")});'
    : kind === 'failure'
      ? 'store("counter", 99); text(await tools.effect({kind:"read-one"})); throw new Error("SCRIPT_FAILURE");'
      : 'store("counter", 99); await tools.effect({kind:"poison"});';
  sendCompleted(socket, 'initial-cell', [{ type: 'custom_tool_call', call_id: 'owned-cell', name: 'exec', input: source }]);
  await until(m => m.owner === owner && m.type === (kind === 'abort' ? 'dispatch' : 'failure'));
  if (kind !== 'abort') assert.equal(acknowledgementLost, true);
  assert.deepEqual(await journal.snapshotStore(sessionId), kind === 'success' ? [['counter', 1]] : []);
  await worker.terminate(); socket.terminate();
  ({ worker, owner } = start());
  socket = await server.nextConnection(); reader = messageReader(socket);
  const recovered = await reader.next();
  const cell = recovered.input.find(item => item.type === 'custom_tool_call_output' && item.call_id === 'owned-cell');
  assert.ok(cell, 'Rust WASM receives reconciled cell receipt');
  const encoded = typeof cell.output === 'string' ? cell.output : cell.output.map(item => item.text ?? '').join('');
  assert.match(encoded, kind === 'success' ? /"counter":1/ : kind === 'failure' ? /SCRIPT_FAILURE/ : /outcome unknown/);
  assert.deepEqual(dispatches, [kind === 'abort' ? 'poison' : 'read-one']);
  sendFinal(socket, 'original-result', 'ORIGINAL_RECOVERED');
  assert.equal((await until(m => m.owner === owner && m.type === 'result')).finalMessage, 'ORIGINAL_RECOVERED');
  await reader.next();
  sendCompleted(socket, 'read-state', [{ type: 'custom_tool_call', call_id: 'read-cell', name: 'exec', input: 'text({counter:load("counter") ?? 0});' }]);
  const follow = await reader.next();
  const output = follow.input.filter(item => item.type === 'custom_tool_call_output' && item.call_id === 'read-cell').at(-1);
  assert.ok(output);
  assert.match(typeof output.output === 'string' ? output.output : output.output.map(item => item.text ?? '').join(''), new RegExp('"counter":' + (kind === 'success' ? 1 : 0)));
  sendFinal(socket, 'follow-result', 'STATE_VERIFIED');
  assert.equal((await until(m => m.owner === owner && m.type === 'follow-on')).finalMessage, 'STATE_VERIFIED');
  t.diagnostic(JSON.stringify({ kind, original: 'ORIGINAL_RECOVERED', follow: 'STATE_VERIFIED', entries: await journal.snapshotStore(sessionId), dispatches }));
});
