import { env, runInDurableObject, SELF } from "cloudflare:test";
import { expect, it } from "vitest";
import { fixtureKeys } from "./fixtures/auth";
import type { Session } from "../src/index";

it("recovers the native original-ID pending stage after losing the host readiness marker", async () => {
  const authorization = `Bearer ${fixtureKeys["fixture-user"]}`;
  expect((await SELF.fetch("https://api.test/v1/credentials/openai", {
    method: "PUT", headers: { authorization }, body: JSON.stringify({ value: "sk-fixture-only" }),
  })).status).toBe(204);
  const created = await SELF.fetch("https://api.test/v1/agents", {
    method: "POST", headers: { authorization, "content-type": "application/json" },
    body: JSON.stringify({ async_tools: true }),
  });
  expect(created.status).toBe(201);
  const { agent_id: agentId } = await created.json<{ agent_id: string }>();
  const stub = (env as unknown as { SESSIONS: DurableObjectNamespace<Session> }).SESSIONS
    .getByName(`fixture-user:${agentId}`);
  // Ensure the async job table exists before installing the fault trigger.
  expect((await SELF.fetch(`https://api.test/v1/agents/${agentId}/jobs`, {
    headers: { authorization },
  })).status).toBe(200);
  // Drop exactly the host SQL readiness marker; the Rust original-call pending
  // output remains authoritative. A provisional handler return alone is never
  // sufficient to execute the mutable-classified Bash operation.
  await runInDurableObject(stub, (_session, state) => state.storage.sql.exec(`
    CREATE TRIGGER lose_pending_stage_ready BEFORE UPDATE OF ready_at ON async_jobs
      WHEN NEW.ready_at IS NOT NULL BEGIN SELECT RAISE(IGNORE); END`));
  const turnId = crypto.randomUUID();
  const admitted = await SELF.fetch(`https://api.test/v1/agents/${agentId}/turns`, {
    method: "POST", headers: { authorization, "content-type": "application/json", "idempotency-key": turnId },
    body: JSON.stringify({ input: "Use async exec_command fixture once, then report its result." }),
  });
  expect(admitted.status).toBe(202);
  const turnUrl = `https://api.test/v1/agents/${agentId}/turns/${turnId}`;
  await expect.poll(async () => (await (await SELF.fetch(turnUrl, { headers: { authorization } }))
    .json<{ state: string; message: string }>()), { timeout: 15_000, interval: 100 })
    .toMatchObject({ state: "completed", message: "Waiting for async Bash" });
  const [job] = await (await SELF.fetch(`https://api.test/v1/agents/${agentId}/jobs`, {
    headers: { authorization },
  })).json<{ job_id: string }[]>();
  expect(job).toBeDefined();
  const before = await runInDurableObject(stub, (_session, state) => state.storage.sql.exec<{
    state: string; ready_at: number | null; attempts: number; call_id: string;
  }>("SELECT state, ready_at, attempts, call_id FROM async_jobs WHERE id = ?", job!.job_id).toArray()[0]);
  expect(before).toMatchObject({ state: "queued", ready_at: null, attempts: 0, call_id: "call-bash" });
  await runInDurableObject(stub, async (_session, state) => {
    state.storage.sql.exec("DROP TRIGGER lose_pending_stage_ready");
    // The test also drops the failed recovery probe's host marker, as a crash
    // between proof and its host commit would. The native proof is untouched.
    state.storage.sql.exec("UPDATE async_jobs SET stage_probe_at = NULL WHERE id = ?", job!.job_id);
    await state.storage.deleteAlarm();
  });
  // Model-call event has already fired, so this alarm has no volatile witness.
  // It must consult the source-scoped native durable pending proof instead.
  await runInDurableObject(stub, async (session) => session.alarm());
  const statusUrl = `https://api.test/v1/agents/${agentId}/jobs/${job!.job_id}`;
  await expect.poll(async () => (await (await SELF.fetch(statusUrl, { headers: { authorization } }))
    .json<{ state: string }>()).state, { timeout: 30_000, interval: 100 }).toBe("delivered");
  const after = await runInDurableObject(stub, (_session, state) => ({
    row: state.storage.sql.exec<{ state: string; attempts: number; original_turn: string; call_id: string }>(
      "SELECT state, attempts, original_turn, call_id FROM async_jobs WHERE id = ?", job!.job_id).toArray()[0],
    turns: state.storage.sql.exec<{ n: number }>("SELECT COUNT(*) AS n FROM turns").toArray()[0]!.n,
  }));
  expect(after.row).toMatchObject({ state: "delivered", original_turn: turnId, call_id: "call-bash", attempts: 1 });
  expect(after.turns).toBe(1);
}, 45_000);

it("delivers a cancelled original-ID terminal after lost readiness without running mutable Bash", async () => {
  const authorization = `Bearer ${fixtureKeys["fixture-user"]}`;
  expect((await SELF.fetch("https://api.test/v1/credentials/openai", {
    method: "PUT", headers: { authorization }, body: JSON.stringify({ value: "sk-fixture-only" }),
  })).status).toBe(204);
  const created = await SELF.fetch("https://api.test/v1/agents", {
    method: "POST", headers: { authorization, "content-type": "application/json" },
    body: JSON.stringify({ async_tools: true }),
  });
  expect(created.status).toBe(201);
  const { agent_id: agentId } = await created.json<{ agent_id: string }>();
  const stub = (env as unknown as { SESSIONS: DurableObjectNamespace<Session> }).SESSIONS
    .getByName(`fixture-user:${agentId}`);
  const jobsUrl = `https://api.test/v1/agents/${agentId}/jobs`;
  expect((await SELF.fetch(jobsUrl, { headers: { authorization } })).status).toBe(200);
  await runInDurableObject(stub, (_session, state) => state.storage.sql.exec(`
    CREATE TRIGGER lose_cancel_ready BEFORE UPDATE OF ready_at ON async_jobs
      WHEN NEW.ready_at IS NOT NULL BEGIN SELECT RAISE(IGNORE); END`));
  const turnId = crypto.randomUUID();
  const admitted = await SELF.fetch(`https://api.test/v1/agents/${agentId}/turns`, {
    method: "POST", headers: { authorization, "content-type": "application/json", "idempotency-key": turnId },
    body: JSON.stringify({ input: "Use async exec_command fixture once, then report its result." }),
  });
  expect(admitted.status).toBe(202);
  await expect.poll(async () => (await (await SELF.fetch(
    `https://api.test/v1/agents/${agentId}/turns/${turnId}`, { headers: { authorization } },
  )).json<{ state: string }>()).state, { timeout: 15_000, interval: 100 }).toBe("completed");
  const [job] = await (await SELF.fetch(jobsUrl, { headers: { authorization } }))
    .json<{ job_id: string; state: string }[]>();
  expect(job).toMatchObject({ state: "queued" });
  const cancelled = await SELF.fetch(`${jobsUrl}/${job!.job_id}`, {
    method: "DELETE", headers: { authorization },
  });
  expect(cancelled.status).toBe(200);
  expect(await cancelled.json()).toMatchObject({ state: "cancelled" });
  await runInDurableObject(stub, (_session, state) => {
    state.storage.sql.exec("DROP TRIGGER lose_cancel_ready");
    state.storage.sql.exec("UPDATE async_jobs SET stage_probe_at = NULL WHERE id = ?", job!.job_id);
  });
  await runInDurableObject(stub, async (session) => session.alarm());
  await expect.poll(async () => (await (await SELF.fetch(`${jobsUrl}/${job!.job_id}`, {
    headers: { authorization },
  })).json<{ state: string }>()).state, { timeout: 30_000, interval: 100 }).toBe("delivered");
  const final = await runInDurableObject(stub, (_session, state) => ({
    row: state.storage.sql.exec<{ attempts: number; terminal_state: string; original_turn: string; call_id: string }>(
      "SELECT attempts, terminal_state, original_turn, call_id FROM async_jobs WHERE id = ?", job!.job_id).toArray()[0],
    turns: state.storage.sql.exec<{ n: number }>("SELECT COUNT(*) AS n FROM turns").toArray()[0]!.n,
  }));
  expect(final.row).toMatchObject({ attempts: 0, terminal_state: "cancelled", original_turn: turnId, call_id: "call-bash" });
  expect(final.turns).toBe(1);
}, 45_000);
