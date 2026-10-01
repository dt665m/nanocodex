import { env, runInDurableObject } from "cloudflare:test";
import { expect, it } from "vitest";
import type { DurableAgentSession } from "../src/index";
import { createManagedCodeEffectJournal } from "../src/managed-recovery-safety";
import { createCloudflareDurabilityStore } from "nanocodex/durability/cloudflare";
import { durabilityRevision } from "nanocodex/durability";
import { managedCodeEvaluator } from "../src/code-evaluator";
// Public JavaScript runtime subpath does not publish a declaration file.
// @ts-expect-error Integration exercises the shipped runtime and real QuickJS.
import { createCodeRuntime } from "nanocodex-tools/runtime/code-runtime";

// Exact pending head from real Rust/WASM's no-journal owned SDK abrupt-restart
// negative control in code-recovery-wasm.test.mjs (revision6, format4).
// This seeds only metadata: shipped QuickJS + Workers SQLite admission is real;
// this journey does not pretend to reconstruct the Rust execution owner.
const LEGACY_HEAD = "{\"nanocodex_durable_state\":{\"format\":4,\"operations\":{\"original\":{\"continuation\":\"3c5c10801f261a05de2f1c96ed2de92bd2c585c2e0c60fb9d34a49aa0ef5f659\",\"retired_model_calls\":0,\"retired_steers\":0,\"input\":\"881eadb99c9a3c4e1d58bfd4e1569080ff079eb944a06be1bbf6a79151b1a998\",\"status\":\"pending\",\"steps\":{\"model-1\":{\"kind\":\"model_call\",\"input\":\"74234e98afe7498fb5daf1f36ac2d78acc339464f950703b8c019892f982b90b\",\"status\":{\"completed\":\"9f977b7a24a53e7d2ea5bad27008a0b5f63ee3d812cf07a280da3f3f63b374ed\"},\"attempts\":1},\"tool-1-owned-cell\":{\"kind\":\"tool_call\",\"input\":\"3c70640a0a05d2050b160eb3ecda5a76acf05676101e07a9e7300689a20b2b3d\",\"status\":\"effect_pending\",\"attempts\":1}},\"accepted_order\":1}},\"latest_checkpoint\":null}}";
const sessions = () => (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;
async function seedLegacyHead(storage: DurableObjectStorage) {
  const store = createCloudflareDurabilityStore(storage);
  storage.sql.exec(`CREATE TABLE IF NOT EXISTS nanocodex_cloudflare_agent (
    singleton INTEGER PRIMARY KEY, session_id TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS nanocodex_cloudflare_durability (
    singleton INTEGER PRIMARY KEY, state_id TEXT NOT NULL)`);
  storage.sql.exec("INSERT OR REPLACE INTO nanocodex_cloudflare_agent VALUES (1, 'fixture-session')");
  storage.sql.exec("INSERT OR REPLACE INTO nanocodex_cloudflare_durability VALUES (1, 'fixture-root')");
  await store.importState("fixture-root", { revision: durabilityRevision("6"), payload: LEGACY_HEAD });
  // The pristine test DO was constructed before we seeded old owner metadata.
  // Erase only its marker to model FIRST rollout onto this preexisting head.
  storage.sql.exec("DELETE FROM managed_code_effect_migration");
}

it("reuses completed nested results and fences an uncertain effect across managed owner loss", async () => {
  const namespace = (env as unknown as { NANOCODEX_SESSIONS: DurableObjectNamespace<DurableAgentSession> }).NANOCODEX_SESSIONS;
  await runInDurableObject(namespace.getByName(crypto.randomUUID()), async (_instance, ctx) => {
    const counts = { one: 0, two: 0, write: 0 };
    let release!: () => void;
    let entered!: () => void;
    const started = new Promise<void>(resolve => { entered = resolve; });
    const pending = new Promise<void>(resolve => { release = resolve; });
    const tools = {
      one: { handler: async () => { counts.one++; return { value: "first durable result" }; } },
      two: { handler: async () => { counts.two++; return { value: "second durable result" }; } },
      write: { handler: async () => { counts.write++; entered(); await pending; return "merchant accepted"; } },
    };
    const source = 'text((await tools.one({})).value); text((await tools.two({})).value); await tools.write({ operation_id: "fixture-original-operation" });';
    const original = createCodeRuntime(tools, { evaluate: managedCodeEvaluator(), effectJournal: createManagedCodeEffectJournal(ctx.storage) });
    const old = original.executeCode(source, "fixture-session", "fixture-cell").then(() => "completed", () => "interrupted");
    await started;
    expect(counts).toEqual({ one: 1, two: 1, write: 1 });
    expect(ctx.storage.sql.exec("SELECT state FROM managed_code_effects ORDER BY call_id").toArray())
      .toEqual([{ state: "completed" }, { state: "completed" }, { state: "pending" }]);
    // Fence the old owner before its response arrives. The effects already
    // happened; a new QuickJS session may replay only acknowledged receipts.
    const recoveredJournal = createManagedCodeEffectJournal(ctx.storage);
    release();
    expect(await old).toBe("interrupted");
    const recovered = createCodeRuntime(tools, { evaluate: managedCodeEvaluator(), effectJournal: recoveredJournal });
    const result = JSON.parse(await recovered.executeCode(source, "fixture-session", "fixture-cell"));
    expect(result.success).toBe(false);
    expect(result.output).toContain("outcome unknown");
    expect(counts).toEqual({ one: 1, two: 1, write: 1 });
    expect(result.nested_calls.slice(0, 2).map((call: { structured_result: unknown }) => call.structured_result))
      .toEqual([{ value: "first durable result" }, { value: "second durable result" }]);
    expect(ctx.storage.sql.exec("SELECT state FROM managed_code_effects WHERE call_id='fixture-cell/code-3'").one()).toEqual({ state: "pending" });
    const next = JSON.parse(await recovered.executeCode('text((await tools.one({})).value);', "fixture-session", "fixture-next-cell"));
    expect(next.success).toBe(true);
    expect(counts.one).toBe(2);
    console.log("CODE_EFFECT_RECOVERY_JOURNEY", JSON.stringify({ counts, result, next }));
    await ctx.storage.deleteAlarm();
  });
}, 30_000);

it("fences the exact legacy parent after >512 noise/archive deletion and retains its fence across owners", async () => {
  await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (_instance, ctx) => {
    await seedLegacyHead(ctx.storage);
    // Old global2 is different from the new cell-local1. This old telemetry is
    // metadata-only because running a removed old host is not this test's remit.
    ctx.storage.sql.exec("INSERT INTO managed_events (turn_id,message_json,created_at) VALUES (?,?,?)", "legacy",
      JSON.stringify({ type: "event", event: { type: "tool.call", payload: { call_id: "owned-cell/code-2" } } }), Date.now());
    for (let index = 0; index < 600; index++) ctx.storage.sql.exec(
      "INSERT INTO managed_events (turn_id,message_json,created_at) VALUES (?,?,?)", "noise",
      JSON.stringify({ type: "event", event: { type: "run.started", payload: { index } } }), Date.now());
    ctx.storage.sql.exec("DELETE FROM managed_events WHERE turn_id='legacy'");
    expect(ctx.storage.sql.exec("SELECT COUNT(*) AS count FROM managed_events").one()).toEqual({ count: 600 });
    const sdkOwners = ctx.storage.sql.exec("SELECT * FROM nanocodex_durable_owners").toArray();
    let writes = 0;
    const tools = { write: { handler: async () => { writes++; return "accepted"; } } };
    const make = () => createCodeRuntime(tools, { evaluate: managedCodeEvaluator(), effectJournal: createManagedCodeEffectJournal(ctx.storage) });
    const source = 'await Promise.all([tools.write({ operation_id: "legacy-original-operation" }), tools.write({ operation_id: "other" })]);';
    const legacy = JSON.parse(await make().executeCode(source, "fixture-session", "owned-cell"));
    expect(legacy.success).toBe(false);
    expect(legacy.output).toContain("outcome unknown");
    expect(legacy.nested_calls[0].call_id).toBe("owned-cell/code-1");
    expect(writes).toBe(0);
    expect(ctx.storage.sql.exec("SELECT * FROM managed_code_effect_legacy_parents").toArray())
      .toEqual([{ session_id: "fixture-session", parent_call_id: "owned-cell", state_id: "fixture-root", step_key: "tool-1-owned-cell" }]);
    // First unknown journal intent must not erase the parent fence on restart.
    ctx.storage.sql.exec("DELETE FROM nanocodex_durable_states");
    const repeated = JSON.parse(await make().executeCode(source, "fixture-session", "owned-cell"));
    expect(repeated.output).toContain("outcome unknown");
    expect(ctx.storage.sql.exec("SELECT state FROM managed_code_effects WHERE call_id='owned-cell/code-2'").one()).toEqual({ state: "pending" });
    const next = JSON.parse(await make().executeCode(source, "fixture-session", "new-cell"));
    expect(next.success).toBe(true);
    const newSession = JSON.parse(await make().executeCode(source, "new-session", "owned-cell"));
    expect(newSession.success).toBe(true);
    expect(writes).toBe(4);
    expect(ctx.storage.sql.exec("SELECT * FROM nanocodex_durable_owners").toArray()).toEqual(sdkOwners);
    console.log("CODE_EFFECT_ARCHIVE_FREE_UPGRADE_JOURNEY", JSON.stringify({ writes, legacy, repeated, next, newSession }));
    await ctx.storage.deleteAlarm();
  });
}, 30_000);

it("replays a journalled completed ordinal and executes a new ordinal under a pending outer head", async () => {
  await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (_instance, ctx) => {
    let reads = 0; let writes = 0;
    let entered!: () => void; let release!: () => void;
    const paused = new Promise<void>(resolve => { entered = resolve; });
    const lostOwner = new Promise<void>(resolve => { release = resolve; });
    const journal = createManagedCodeEffectJournal(ctx.storage);
    const tools = { one: { handler: async () => { reads++; return "completed"; } }, write: { handler: async () => { writes++; return "new ordinal"; } } };
    const source = 'text(await tools.one({})); text(await tools.write({}));';
    const original = createCodeRuntime(tools, { evaluate: managedCodeEvaluator(), effectJournal: {
      ...journal, async begin(context: Parameters<typeof journal.begin>[0]) {
        // Unavoidable owner-loss injection immediately before the next intent,
        // not a fake tool result: the first receipt is real and durably stored.
        if (context.callId.endsWith("/code-2")) { entered(); await lostOwner; throw new Error("fixture owner lost before next intent"); }
        return journal.begin(context);
      },
    } });
    const old = original.executeCode(source, "fixture-session", "owned-cell").catch(() => "interrupted");
    await paused;
    expect({ reads, writes }).toEqual({ reads: 1, writes: 0 });
    await seedLegacyHead(ctx.storage);
    const replacement = createManagedCodeEffectJournal(ctx.storage);
    release(); await old;
    expect(ctx.storage.sql.exec("SELECT * FROM managed_code_effect_legacy_parents").toArray()).toEqual([]);
    const recovered = createCodeRuntime(tools, { evaluate: managedCodeEvaluator(), effectJournal: replacement });
    const result = JSON.parse(await recovered.executeCode(source, "fixture-session", "owned-cell"));
    expect(result.success).toBe(true);
    expect({ reads, writes }).toEqual({ reads: 1, writes: 1 });
    console.log("CODE_EFFECT_JOURNALLED_PARENT_UPGRADE_JOURNEY", JSON.stringify({ reads, writes, result }));
    await ctx.storage.deleteAlarm();
  });
}, 30_000);

it("fences orphan child-head pending parents and old child sessions but permits fresh identities", async () => {
  await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (_instance, ctx) => {
    await seedLegacyHead(ctx.storage);
    await createCloudflareDurabilityStore(ctx.storage).importState("orphan-child-head", { revision: durabilityRevision("6"), payload: LEGACY_HEAD });
    ctx.storage.sql.exec(`CREATE TABLE nanocodex_cloudflare_subagents (session_id TEXT PRIMARY KEY,
      agent_id TEXT NOT NULL, descriptor_json TEXT NOT NULL, host_context_ref TEXT);
      INSERT INTO nanocodex_cloudflare_subagents VALUES ('retained-child', '1', '{}', NULL);
      CREATE TABLE nanocodex_cloudflare_subagent_checkpoints (chunk_index INTEGER PRIMARY KEY, payload TEXT NOT NULL)
    `);
    // Minimal legacy checkpoint identity metadata, from the removed SDK schema;
    // no guessed runtime state or reconstructed child outputs are consumed.
    ctx.storage.sql.exec("INSERT INTO nanocodex_cloudflare_subagent_checkpoints VALUES (0, ?)",
      JSON.stringify({ root_session_id: "fixture-session", children: [{ descriptor: { session_id: "checkpoint-child" } }] }));
    let writes = 0;
    const tools = { write: { handler: async () => { writes++; return "accepted"; } } };
    const make = () => createCodeRuntime(tools, { evaluate: managedCodeEvaluator(), effectJournal: createManagedCodeEffectJournal(ctx.storage) });
    const source = 'await tools.write({ operation_id: "child-original-operation" });';
    const orphan = JSON.parse(await make().executeCode(source, "unlinked-child", "owned-cell"));
    const retained = JSON.parse(await make().executeCode(source, "retained-child", "unknown-parent"));
    const checkpointChild = JSON.parse(await make().executeCode(source, "checkpoint-child", "checkpoint-parent"));
    expect(orphan.output).toContain("outcome unknown");
    expect(retained.output).toContain("outcome unknown");
    expect(checkpointChild.output).toContain("outcome unknown");
    expect(writes).toBe(0);
    ctx.storage.sql.exec("DROP TABLE nanocodex_cloudflare_subagents; DROP TABLE nanocodex_cloudflare_subagent_checkpoints; DELETE FROM nanocodex_durable_states");
    const persisted = JSON.parse(await make().executeCode(source, "retained-child", "another-parent"));
    expect(persisted.output).toContain("outcome unknown");
    const fresh = JSON.parse(await make().executeCode(source, "fresh-child", "new-parent"));
    expect(fresh.success).toBe(true);
    expect(writes).toBe(1);
    console.log("CODE_EFFECT_CHILD_UPGRADE_JOURNEY", JSON.stringify({ writes, orphan, retained, checkpointChild, persisted, fresh }));
    await ctx.storage.deleteAlarm();
  });
}, 30_000);

it.each(["corrupt", "unsupported", "oversized"])("fails closed on a %s existing head without hydrating unbounded state", async kind => {
  await runInDurableObject(sessions().getByName(crypto.randomUUID()), async (_instance, ctx) => {
    await seedLegacyHead(ctx.storage);
    const payload = kind === "oversized" ? "x".repeat(1024 * 1024 + 1)
      : kind === "unsupported" ? LEGACY_HEAD.replace('"format":4', '"format":5') : "{corrupt";
    ctx.storage.sql.exec("UPDATE nanocodex_durable_states SET payload=?", payload);
    let writes = 0; let headReads = 0;
    const storage = {
      sql: { exec(sql: string, ...args: Array<string | number | null>) {
        if (sql.startsWith("SELECT revision, payload FROM nanocodex_durable_states")) headReads++;
        return ctx.storage.sql.exec(sql, ...args);
      } },
      transactionSync: <T>(callback: () => T) => ctx.storage.transactionSync(callback),
      sync: () => ctx.storage.sync(),
    } as DurableObjectStorage;
    const tools = { write: { handler: async () => { writes++; return "must not dispatch"; } } };
    const make = () => createCodeRuntime(tools, { evaluate: managedCodeEvaluator(), effectJournal: createManagedCodeEffectJournal(storage) });
    const source = 'await tools.write({});';
    const result = JSON.parse(await make().executeCode(source, "fixture-session", "owned-cell"));
    expect(result.output).toContain("outcome unknown");
    expect(writes).toBe(0);
    expect(headReads).toBe(kind === "oversized" ? 0 : 1);
    const readsAfterSnapshot = headReads;
    const next = JSON.parse(await make().executeCode(source, "fixture-session", "different-cell"));
    expect(next.output).toContain("outcome unknown");
    expect(headReads).toBe(readsAfterSnapshot);
    expect(writes).toBe(0);
    console.log("CODE_EFFECT_UNREADABLE_HEAD_JOURNEY", JSON.stringify({ kind, writes, headReads, result, next }));
    await ctx.storage.deleteAlarm();
  });
}, 30_000);
