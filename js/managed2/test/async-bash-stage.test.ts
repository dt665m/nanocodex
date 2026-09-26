import { env, runInDurableObject, SELF } from "cloudflare:test";
import { expect, it } from "vitest";
import { fixtureKeys } from "./fixtures/auth";
import type { Session } from "../src/index";

it("executes a mutable-classified Bash call only after its native original-ID pending stage", async () => {
  const authorization = `Bearer ${fixtureKeys["fixture-user"]}`;
  expect((await SELF.fetch("https://api.test/v1/credentials/openai", {
    method: "PUT", headers: { authorization }, body: JSON.stringify({ value: "sk-fixture-only" }),
  })).status).toBe(204);
  const created = await SELF.fetch("https://api.test/v1/agents", {
    method: "POST", headers: { authorization, "content-type": "application/json" },
    body: JSON.stringify({ input: "Use async exec_command fixture once, then report its result.", async_tools: true }),
  });
  expect(created.status).toBe(202);
  const { agent_id: agentId, turn_id: turnId } = await created.json<{ agent_id: string; turn_id: string }>();
  const stub = (env as unknown as { SESSIONS: DurableObjectNamespace<Session> }).SESSIONS
    .getByName(`fixture-user:${agentId}`);
  const turnUrl = `https://api.test/v1/agents/${agentId}/turns/${turnId}`;
  await expect.poll(async () => (await (await SELF.fetch(turnUrl, { headers: { authorization } }))
    .json<{ state: string; message: string }>()), { timeout: 15_000, interval: 100 })
    .toMatchObject({ state: "completed", message: "Waiting for async Bash" });
  const [job] = await (await SELF.fetch(`https://api.test/v1/agents/${agentId}/jobs`, {
    headers: { authorization },
  })).json<{ job_id: string }[]>();
  expect(job).toBeDefined();
  await expect.poll(async () => (await (await SELF.fetch(
    `https://api.test/v1/agents/${agentId}/jobs/${job!.job_id}`, { headers: { authorization } },
  )).json<{ state: string }>()).state, { timeout: 30_000, interval: 100 }).toBe("delivered");
  const rows = await runInDurableObject(stub, (_session, state) => ({
    job: state.storage.sql.exec<{ state: string; call_id: string; original_turn: string;
      attempts: number; ready_at: number; started_at: number }>(
      "SELECT state, call_id, original_turn, attempts, ready_at, started_at FROM async_jobs WHERE id = ?",
      job!.job_id).toArray()[0],
    timing: state.storage.sql.exec<{ first_tool_result_ms: number; post_tool_model_call_ms: number }>(
      "SELECT first_tool_result_ms, post_tool_model_call_ms FROM turn_timing WHERE id = ?", turnId).toArray()[0],
  }));
  expect(rows.job).toMatchObject({ state: "delivered", call_id: "call-bash", original_turn: turnId, attempts: 1 });
  expect(rows.job.ready_at).toBeGreaterThan(0);
  expect(rows.job.started_at).toBeGreaterThanOrEqual(rows.job.ready_at);
  expect(rows.timing.post_tool_model_call_ms).toBeGreaterThanOrEqual(rows.timing.first_tool_result_ms);
}, 40_000);
