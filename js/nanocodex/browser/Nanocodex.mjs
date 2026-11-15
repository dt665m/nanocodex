import { create as createAgent } from "./Agent.mjs";
import { initializeBrowserEngine } from "./engine.mjs";
import { createNanocodexFactory } from "../runtime/nanocodex.mjs";

/**
 * Configures backends once. A Codex-only runtime places each root in its own
 * package module Worker; mixed-family or Claude roots run in this page isolate.
 */
export const create = createNanocodexFactory({
  placement: "browser",
  createAgent,
  async loadCatalog({ module }) {
    await initializeBrowserEngine({ module });
    return (await import("../pkg-web/nanocodex.js")).modelCapabilities();
  },
});
