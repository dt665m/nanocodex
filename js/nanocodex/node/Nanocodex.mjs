import { createRequire } from "node:module";

import { create as createAgent } from "./Agent.mjs";
import { createNanocodexFactory } from "../runtime/nanocodex.mjs";

/** Configures backends once; every spawn is an independent Node WASM root Agent. */
export const create = createNanocodexFactory({
  placement: "node",
  createAgent,
  async loadCatalog({ module }) {
    if (module === undefined) return createRequire(import.meta.url)("../pkg-node/nanocodex.js").modelCapabilities();
    const { initializeBrowserEngine } = await import("../browser/engine.mjs");
    await initializeBrowserEngine({ module });
    return (await import("../pkg-web/nanocodex.js")).modelCapabilities();
  },
});
