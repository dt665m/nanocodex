import { create as createAgent } from "../browser/InlineAgent.mjs";
import { initializeBrowserEngine } from "../browser/engine.mjs";
import { createNanocodexFactory } from "../runtime/nanocodex.mjs";

/** Configures backends once; every spawn runs inline in the current Web API isolate. */
export const create = createNanocodexFactory({
  placement: "host",
  createAgent,
  requiresEvaluator: () => typeof globalThis.Worker !== "function",
  async loadCatalog({ module }) {
    await initializeBrowserEngine({ module });
    return (await import("../pkg-web/nanocodex.js")).modelCapabilities();
  },
});
