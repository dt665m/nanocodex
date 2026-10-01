import type { AgentEvent, CodeEffectContext, CodeEffectJournal, CodeEffectReceipt } from "nanocodex";
import { createCloudflareDurabilityStore } from "nanocodex/durability/cloudflare";
import { createHash } from "node:crypto";
import { inputChunks } from "./managed-turn-input";

// Caught transient retries aren't owner loss. This independent budget counts
// admissions whose dispatch lease wasn't settled and ignores projected run IDs.
const MAX_MANAGED_ABRUPT_ATTEMPTS = 3;
export const MANAGED_RECOVERY_UNKNOWN = "MANAGED_RECOVERY_EXHAUSTED: repeated runtime loss while recovering the same unfinished operation; execution outcome unknown. Automatic replay was stopped; original operation identity and receipts were retained. Inspect retained tool receipts or external state before retrying any effect with its original operation identity.";

export class ManagedRecoverySafety {
  constructor(readonly storage: DurableObjectStorage) {
    storage.sql.exec(`CREATE TABLE IF NOT EXISTS managed_recovery_safety (
      turn_id TEXT PRIMARY KEY, armed INTEGER NOT NULL DEFAULT 0,
      abrupt_attempts INTEGER NOT NULL DEFAULT 0, stopped INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE IF NOT EXISTS managed_recovery_progress (
      turn_id TEXT NOT NULL, progress_key TEXT NOT NULL, PRIMARY KEY (turn_id, progress_key)
    )`);
  }

  begin(id: string): boolean {
    return this.storage.transactionSync(() => {
      const inserted = this.storage.sql.exec("INSERT OR IGNORE INTO managed_recovery_safety (turn_id) VALUES (?) RETURNING turn_id", id).toArray();
      const row = this.storage.sql.exec<{ armed: number; abrupt_attempts: number; stopped: number }>(
        "SELECT armed, abrupt_attempts, stopped FROM managed_recovery_safety WHERE turn_id = ?", id,
      ).one();
      const attempts = row.armed ? row.abrupt_attempts + 1 : 1;
      // Do not cancel every in-flight operation on the first upgraded owner.
      // Only an observed legacy poison loop or repeated owner loss exhausts it.
      const stopped = row.stopped === 1 || (inserted.length > 0 && this.legacyPoisonLoop(id))
        || attempts > MAX_MANAGED_ABRUPT_ATTEMPTS;
      this.storage.sql.exec(`UPDATE managed_recovery_safety SET armed = 1,
        abrupt_attempts = ?, stopped = ? WHERE turn_id = ?`, attempts, stopped ? 1 : 0, id);
      return stopped;
    });
  }

  private legacyPoisonLoop(id: string): boolean {
    // Bounded metadata-only SQL; avoid materializing event bodies in JS. Call
    // identities stay stable while projected request/run identities change.
    return this.storage.sql.exec<{ call_id: string }>(`SELECT
      json_extract(message_json, '$.event.payload.call_id') AS call_id
      FROM (SELECT message_json FROM managed_events WHERE turn_id = ?
        ORDER BY cursor DESC LIMIT 256)
      WHERE json_valid(message_json) AND json_extract(message_json, '$.type') = 'event'
        AND json_extract(message_json, '$.event.type') IN ('tool.call', 'tool.call.started')
        AND json_extract(message_json, '$.event.payload.call_id') IS NOT NULL
      GROUP BY call_id HAVING COUNT(*) >= 3 LIMIT 1`, id).toArray().length > 0;
  }

  stopped(id: string): boolean {
    return this.storage.sql.exec<{ stopped: number }>(
      "SELECT stopped FROM managed_recovery_safety WHERE turn_id = ?", id,
    ).toArray()[0]?.stopped === 1;
  }

  settle(id: string): void {
    this.storage.sql.exec(`UPDATE managed_recovery_safety SET armed = 0,
      abrupt_attempts = CASE WHEN stopped = 1 THEN abrupt_attempts ELSE 0 END WHERE turn_id = ?`, id);
  }

  progress(id: string, event: AgentEvent): void {
    // Recovered run/model events aren't progress. A call's first durable result
    // moves the unfinished effect boundary; duplicate result replay cannot.
    if (event.type !== "tool.result" || typeof event.payload.call_id !== "string") return;
    const key = event.payload.call_id;
    const inserted = this.storage.sql.exec(`INSERT OR IGNORE INTO managed_recovery_progress
      (turn_id, progress_key) VALUES (?, ?) RETURNING progress_key`, id, key).toArray();
    if (inserted.length) this.storage.sql.exec(`UPDATE managed_recovery_safety
      SET abrupt_attempts = 0 WHERE turn_id = ? AND stopped = 0`, id);
  }
}

/** Account-private host journal: guest source cannot select or clear receipts.
 * Scope to the original session/cell/ordinal, never projected turn identities.
 * Retain unknown intents and receipts after settlement for reconciliation. */
export function createManagedCodeEffectJournal(storage: DurableObjectStorage): CodeEffectJournal {
  storage.sql.exec(`CREATE TABLE IF NOT EXISTS managed_code_effect_runtime (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1), generation TEXT NOT NULL
  );
  CREATE TABLE IF NOT EXISTS managed_code_effects (
    effect_key TEXT PRIMARY KEY, session_id TEXT NOT NULL, turn_id TEXT,
    parent_call_id TEXT NOT NULL, call_id TEXT NOT NULL, name TEXT NOT NULL,
    input_hash TEXT NOT NULL, generation TEXT NOT NULL, state TEXT NOT NULL,
    receipt_chunks INTEGER, created_at INTEGER NOT NULL, completed_at INTEGER
  );
  CREATE TABLE IF NOT EXISTS managed_code_effect_receipt_chunks (
    effect_key TEXT NOT NULL, chunk_index INTEGER NOT NULL, receipt_json TEXT NOT NULL,
    PRIMARY KEY (effect_key, chunk_index)
  )`);
  // Admission must see the old Rust head before any new runtime callbacks.
  // Event history can be archived; old global code ordinals are not identities.
  snapshotLegacyCodeParents(storage);
  const generation = crypto.randomUUID();
  storage.sql.exec(`INSERT INTO managed_code_effect_runtime VALUES (1, ?)
    ON CONFLICT(singleton) DO UPDATE SET generation = excluded.generation`, generation);
  const assertOwner = () => {
    if (storage.sql.exec<{ generation: string }>(
      "SELECT generation FROM managed_code_effect_runtime WHERE singleton = 1",
    ).one().generation !== generation) throw new Error("Code Mode effect journal owner was fenced; outcome unknown");
  };
  const identity = (context: CodeEffectContext) => {
    if (![context.sessionId, context.parentCallId, context.callId, context.name, context.source]
      .every(value => typeof value === "string" && value.length > 0)) {
      throw new Error("Code Mode effect journal requires original call identity");
    }
    const key = JSON.stringify([context.sessionId, context.parentCallId, context.callId]);
    const hash = createHash("sha256").update(JSON.stringify([context.source, context.name, context.input])).digest("hex");
    return { key, hash };
  };
  type Effect = { input_hash: string; state: string; generation: string; receipt_chunks: number | null };
  const read = (key: string) => storage.sql.exec<Effect>(
    "SELECT input_hash, state, generation, receipt_chunks FROM managed_code_effects WHERE effect_key = ?", key,
  ).toArray()[0];
  return {
    async begin(context) {
      const { key, hash } = identity(context);
      const result = storage.transactionSync(() => {
        assertOwner();
        const existing = read(key);
        if (existing) {
          if (existing.input_hash !== hash) throw new Error("Code Mode effect identity/input conflict; outcome unknown");
          if (existing.state !== "completed") return { status: "unknown" as const };
          const chunks = storage.sql.exec<{ chunk_index: number; receipt_json: string }>(
            "SELECT chunk_index, receipt_json FROM managed_code_effect_receipt_chunks WHERE effect_key = ? ORDER BY chunk_index", key,
          ).toArray();
          if (chunks.length !== existing.receipt_chunks || chunks.some((chunk, index) => chunk.chunk_index !== index)) {
            throw new Error("Code Mode effect receipt is incomplete; outcome unknown");
          }
          return { status: "replay" as const, receipt: JSON.parse(chunks.map(chunk => chunk.receipt_json).join("")) as CodeEffectReceipt };
        }
        const legacy = storage.sql.exec(`SELECT 1 FROM managed_code_effect_legacy_parents
          WHERE session_id IN (?, '') AND parent_call_id = ?
          UNION ALL SELECT 1 FROM managed_code_effect_legacy_sessions
          WHERE session_id IN (?, '') LIMIT 1`,
          context.sessionId, context.parentCallId, context.sessionId).toArray().length > 0;
        storage.sql.exec(`INSERT INTO managed_code_effects
          (effect_key, session_id, turn_id, parent_call_id, call_id, name, input_hash, generation, state, created_at)
          VALUES (?, ?, ?, ?, ?, ?, ?, ?, 'pending', ?)`,
        key, context.sessionId, context.turnId ?? null, context.parentCallId, context.callId,
        context.name, hash, generation, Date.now());
        return legacy ? { status: "unknown" as const } : { status: "execute" as const };
      });
      // A local evaluator can run before output-gated network I/O. Explicitly
      // acknowledge durable intent before dispatching any nested effect.
      await storage.sync();
      assertOwner();
      return result;
    },
    async complete(context, receipt) {
      const { key, hash } = identity(context);
      const encoded = JSON.stringify(receipt);
      // The public SDK has already bounded snapshots before copying. Defend
      // this storage boundary as well, including imported/custom adapters.
      if (encoded.length > 8 * 1024 * 1024
        || new TextEncoder().encode(encoded).byteLength > 8 * 1024 * 1024) {
        throw new Error("Code Mode effect receipt exceeds 8 MiB; outcome unknown");
      }
      storage.transactionSync(() => {
        assertOwner();
        const existing = read(key);
        if (!existing || existing.input_hash !== hash || existing.generation !== generation || existing.state !== "pending") {
          throw new Error("Code Mode effect completion lost its original intent; outcome unknown");
        }
        let count = 0;
        for (const chunk of inputChunks(encoded)) storage.sql.exec(`INSERT INTO managed_code_effect_receipt_chunks
          (effect_key, chunk_index, receipt_json) VALUES (?, ?, ?)`, key, count++, chunk);
        storage.sql.exec(`UPDATE managed_code_effects SET state = 'completed', receipt_chunks = ?, completed_at = ?
          WHERE effect_key = ?`, count, Date.now(), key);
      });
      await storage.sync();
      assertOwner();
    },
  };
}


/** Read only: never acquire an SDK owner just to inspect its execution head.
 * Empty session scope is conservative when an orphan head has no authoritative
 * session link. A corrupt head blocks missing receipts, not receipt replay. */
function snapshotLegacyCodeParents(storage: DurableObjectStorage): void {
  storage.sql.exec(`CREATE TABLE IF NOT EXISTS managed_code_effect_legacy_parents (
    session_id TEXT NOT NULL, parent_call_id TEXT NOT NULL,
    state_id TEXT NOT NULL, step_key TEXT NOT NULL,
    PRIMARY KEY (session_id, parent_call_id)
  );
  CREATE TABLE IF NOT EXISTS managed_code_effect_legacy_sessions (
    session_id TEXT PRIMARY KEY, reason TEXT NOT NULL
  );
  CREATE TABLE IF NOT EXISTS managed_code_effect_migration (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1), version INTEGER NOT NULL
  )`);
  const block = (session: string, reason: string) => storage.sql.exec(
    "INSERT OR IGNORE INTO managed_code_effect_legacy_sessions VALUES (?, ?)", session, reason);
  // One transaction freezes the legacy-parent decision, including the absence
  // of a preexisting journal. Persist it separately: the first unknown intent
  // must not make that legacy parent look journalled on the next owner.
  storage.transactionSync(() => {
    const migrated = storage.sql.exec<{ version: number }>(
      "SELECT version FROM managed_code_effect_migration WHERE singleton = 1",
    ).toArray()[0];
    if (migrated) {
      if (migrated.version !== 1) block("", "unsupported Code Mode migration version; outcome unknown");
      return;
    }
    try {
      const tables = new Set(storage.sql.exec<{ name: string }>(
        "SELECT name FROM sqlite_master WHERE type = 'table'",
      ).toArray().map(row => row.name));
      const rootSession = tables.has("nanocodex_cloudflare_agent")
        ? storage.sql.exec<{ session_id: string }>(
          "SELECT session_id FROM nanocodex_cloudflare_agent WHERE singleton = 1",
        ).toArray()[0]?.session_id : undefined;
      const rootState = tables.has("nanocodex_cloudflare_durability")
        ? storage.sql.exec<{ state_id: string }>(
          "SELECT state_id FROM nanocodex_cloudflare_durability WHERE singleton = 1",
        ).toArray()[0]?.state_id : undefined;
      if ((rootSession !== undefined && (typeof rootSession !== "string" || !rootSession))
        || (rootState !== undefined && (typeof rootState !== "string" || !rootState))
        || (rootState !== undefined && rootSession === undefined)) {
        throw new Error("unreadable root durability identity");
      }
      const store = createCloudflareDurabilityStore(storage);
      // Control metadata only: never hydrate arbitrarily large legacy heads or
      // checkpoint chunks on chat-only cold construction. This one-time bounded
      // migration fails closed rather than introducing another memory loop.
      const budget = storage.sql.exec<{ count: number; bytes: number }>(
        "SELECT COUNT(*) AS count, COALESCE(SUM(length(CAST(payload AS BLOB))), 0) AS bytes FROM nanocodex_durable_states",
      ).one();
      if (budget.count > 128 || budget.bytes > 1024 * 1024) throw new Error("legacy head snapshot exceeds bounded migration budget");
      const states = storage.sql.exec<{ state_id: string }>(
        "SELECT state_id FROM nanocodex_durable_states",
      ).toArray();
      const rootStateId = rootState ?? (rootSession === undefined ? undefined : `cloudflare:${rootSession}`);
      for (const { state_id: stateId } of states) {
        const loaded = store.load(stateId);
        // The public SQLite adapter is synchronous. Fail closed if a future
        // adapter changes this rather than admitting before the snapshot.
        if (loaded instanceof Promise || "then" in loaded) throw new Error("asynchronous durability head");
        if (loaded.payload === null) continue; // Validated revision-zero head.
        const envelope: unknown = JSON.parse(loaded.payload);
        if (!isObject(envelope) || Object.keys(envelope).length !== 1 || !isObject(envelope.nanocodex_durable_state)) throw new Error("unreadable durability head");
        const head = envelope.nanocodex_durable_state;
        if (head.format !== 4 || Object.keys(head).length !== 3 || !isObject(head.operations)
          || !(head.latest_checkpoint === null || typeof head.latest_checkpoint === "string")) {
          throw new Error("unsupported durability head");
        }
        for (const [operationId, operation] of Object.entries(head.operations)) {
          if (!operationId || !isObject(operation) || !isObject(operation.steps)
            || typeof operation.input !== "string" || !operation.input
            || !Number.isSafeInteger(operation.accepted_order) || Number(operation.accepted_order) < 1
            || !Number.isSafeInteger(operation.retired_steers) || Number(operation.retired_steers) < 0
            || (operation.retired_model_calls !== undefined && (!Number.isSafeInteger(operation.retired_model_calls)
              || Number(operation.retired_model_calls) < 0))
            || (operation.continuation !== undefined && operation.continuation !== null && typeof operation.continuation !== "string")
            || !(operation.status === "pending" || (isObject(operation.status)
              && Object.keys(operation.status).length === 1
              && ["completed", "failed", "cancelled"].some(key => isObject(operation.status) && isObject(operation.status[key]))))) {
            throw new Error("unreadable durable operation");
          }
          for (const [stepKey, step] of Object.entries(operation.steps)) {
            if (!stepKey || !isObject(step) || typeof step.kind !== "string" || !step.kind || typeof step.input !== "string" || !step.input
              || !Number.isSafeInteger(step.attempts) || Number(step.attempts) < 1
              || !(step.status === "effect_pending"
                || (isObject(step.status) && Object.keys(step.status).length === 1 && typeof step.status.completed === "string"))) {
              throw new Error("unreadable durable step");
            }
            if (step.kind !== "tool_call" || step.status !== "effect_pending") continue;
            const match = /^tool-[1-9][0-9]*-(.+)$/s.exec(stepKey);
            if (!match) throw new Error("unreadable pending tool identity");
            const session = stateId === rootStateId ? rootSession ?? "" : "";
            const parent = match[1]!;
            // Only a proved exact session/parent may be excluded. An orphan
            // head cannot borrow a different session's journal as authority.
            if (session && storage.sql.exec(`SELECT 1 FROM managed_code_effects
              WHERE session_id = ? AND parent_call_id = ? LIMIT 1`, session, parent).toArray().length) continue;
            storage.sql.exec(`INSERT OR IGNORE INTO managed_code_effect_legacy_parents
              (session_id, parent_call_id, state_id, step_key) VALUES (?, ?, ?, ?)`, session, parent, stateId, stepKey);
          }
        }
      }
      // Current children have no durable execution policy and never rehydrate
      // after owner loss. Old SDKs retained child descriptors/checkpoints; those
      // are not root heads and must not silently inherit the root's safe scope.
      if (tables.has("nanocodex_cloudflare_subagents")) {
        const budget = storage.sql.exec<{ count: number; bytes: number }>(
          "SELECT COUNT(*) AS count, COALESCE(SUM(length(CAST(session_id AS BLOB))), 0) AS bytes FROM nanocodex_cloudflare_subagents",
        ).one();
        if (budget.count > 128 || budget.bytes > 65_536) throw new Error("legacy child identities exceed bounded migration budget");
        for (const { session_id: session } of storage.sql.exec<{ session_id: string }>(
          "SELECT session_id FROM nanocodex_cloudflare_subagents",
        ).toArray()) {
          if (typeof session !== "string" || !session) throw new Error("unreadable retained child identity");
          block(session, "retained child has no authoritative pending execution head");
        }
      }
      if (tables.has("nanocodex_cloudflare_subagent_checkpoints")) {
        const budget = storage.sql.exec<{ count: number; bytes: number }>(
          "SELECT COUNT(*) AS count, COALESCE(SUM(length(CAST(payload AS BLOB))), 0) AS bytes FROM nanocodex_cloudflare_subagent_checkpoints",
        ).one();
        if (budget.count > 16 || budget.bytes > 1024 * 1024) throw new Error("legacy child checkpoint exceeds bounded migration budget");
        const chunks = storage.sql.exec<{ chunk_index: number; payload: string }>(
          "SELECT chunk_index, payload FROM nanocodex_cloudflare_subagent_checkpoints ORDER BY chunk_index",
        ).toArray();
        if (chunks.length) {
          if (chunks.length > 16 || chunks.some((chunk, index) => chunk.chunk_index !== index
            || typeof chunk.payload !== "string" || chunk.payload.length > 65_536)) throw new Error("unreadable child checkpoint");
          const checkpoint: unknown = JSON.parse(chunks.map(chunk => chunk.payload).join(""));
          if (!isObject(checkpoint) || !Array.isArray(checkpoint.children) || checkpoint.children.length > 128) throw new Error("unreadable child checkpoint");
          for (const child of checkpoint.children) {
            if (!isObject(child) || !isObject(child.descriptor)
              || typeof child.descriptor.session_id !== "string" || !child.descriptor.session_id) throw new Error("unreadable checkpoint child identity");
            block(child.descriptor.session_id, "retained child checkpoint lacks authoritative pending execution head");
          }
        }
      }
    } catch {
      block("", "legacy durability head or session lineage unreadable or exceeds migration budget; outcome unknown");
    }
    storage.sql.exec("INSERT INTO managed_code_effect_migration VALUES (1, 1)");
  });
}

function isObject(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
