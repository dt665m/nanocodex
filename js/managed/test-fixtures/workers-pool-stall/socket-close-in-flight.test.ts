import { env } from "cloudflare:test";
import { expect, it } from "vitest";
import { EXEC_COMMAND_PARAMETERS, EXECUTION_OUTPUT_SCHEMA } from "nanocodex-tools/execution-contract";

// Synthetic #951 trigger for test/workers-pool-diagnostics-journey.test.mjs, run by
// vitest.pool-stall.config.ts without test/durable-object-settle.ts. The test
// returns while its Durable Object's webSocketClose event is still in flight,
// so the pool never receives this file's "testfileFinished".
it("closes a Durable Object WebSocket without awaiting the object's close handler", async () => {
  const owner = crypto.randomUUID();
  const stub = (env as unknown as { NANOCODEX_ACCOUNT_TOOLS: DurableObjectNamespace }).NANOCODEX_ACCOUNT_TOOLS.getByName(owner);
  const response = await stub.fetch("https://account-tools.internal/tool-host", {
    headers: { upgrade: "websocket", "x-nanocodex-owner-id": owner },
  });
  const socket = response.webSocket!;
  socket.accept();
  const ready = new Promise(resolve => socket.addEventListener("message", event => resolve(JSON.parse(String(event.data))), { once: true }));
  socket.send(JSON.stringify({
    type: "catalog", capabilities: ["turn_metadata"], attachment_id: "stall-machine",
    tools: [{
      provider: "machine", remote_name: "exec_command", parallel_safe: true, summary: "Machine exec_command", timeout_ms: 30_000,
      definition: { type: "function", name: "exec_command", description: "Machine exec_command", strict: false,
        parameters: EXEC_COMMAND_PARAMETERS, output_schema: EXECUTION_OUTPUT_SCHEMA },
    }],
    machines: [{ id: "stall-machine", name: "Stall machine", workspace: "/app", capabilities: ["shell"] }],
  }));
  await expect(ready).resolves.toEqual({ type: "ready" });
  socket.close();
});
