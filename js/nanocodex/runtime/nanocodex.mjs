// Reusable Nanocodex runtime: configure backends once, then create independent
// root Agents through the existing per-agent Agent.create path. Each root gets
// its own host, tool execution state, code evaluator binding and session; only
// immutable configuration and caller-owned credential capabilities are shared.
import {
  observeAgentRelease,
  retainAgentForOwner,
  shutdownOwnedAgent,
} from "../internal.mjs";
import { claudeTransportOptions, isClaudeTransport } from "./claude-transport.mjs";
import { managedTransportOptions } from "./managed-transport.mjs";
import { resolveResponsesTransport } from "./responses-transport.mjs";
import { tools as workspaceTools } from "./workspace.mjs";

const FAMILIES = Object.freeze(["codex", "claude"]);
const RUNTIME_KEYS = new Set(["backends", "harness", "model", "thinking", "instructions", "workspace", "codeEvaluator", "module"]);
const RUNTIME_REJECTED = Object.freeze({
  tools: "tools belong to each backend: backends.codex.tools / backends.claude.tools",
  codeMode: "codeMode belongs to each backend",
  transport: "transports belong to backends.codex / backends.claude",
  auth: "Claude authentication belongs to backends.claude: Transport.anthropic()",
  harnesses: "Nanocodex.create wires alternate-family children from its backends",
  sessionId: "session identity is per Agent; use Agent.create for a named session",
  durability: "durable sessions are per Agent; use Agent.create with durability",
  durabilityId: "durable sessions are per Agent; use Agent.create with durability",
  resume: "use nano.resume(checkpoint)",
});
const SHARED_BACKEND_KEYS = ["transport", "tools", "codeMode", "workspace"];
const CODEX_KEYS = new Set([...SHARED_BACKEND_KEYS, "filesystem", "filesystemTools", "reasoningMode", "fastMode",
  "instantToolSteering", "rawApiEvents", "additionalInstructions", "mcp", "executionEnvironment",
  "beforeCompaction", "codeEffectJournal"]);
const CLAUDE_KEYS = new Set([...SHARED_BACKEND_KEYS, "filesystem", "maxTokens", "cache", "adaptiveThinking", "keepThinking",
  "fastMode", "parallelTools", "contextWindowTokens", "autoCompactWindowTokens", "subagents"]);
// Root-only Codex settings never become part of an alternate-family child recipe.
const CODEX_CHILD_KEYS = ["reasoningMode", "fastMode"];
const CLAUDE_CHILD_EXCLUDED = new Set(["subagents"]);
// Caller-owned stateful capabilities keep their identity.
const CAPABILITY_KEYS = new Set(["codeEffectJournal", "beforeCompaction"]);
const SPAWN_KEYS = new Set(["instructions", "harness", "model", "thinking", "workspace"]);
const RESUME_KEYS = new Set(["harness", "instructions", "workspace"]);

/**
 * @param {{ placement: string, createAgent(options: object): Promise<object>,
 *   loadCatalog(options: { module?: unknown }): Promise<readonly object[]> | readonly object[],
 *   requiresEvaluator?: () => boolean }} platform
 */
export function createNanocodexFactory(platform) {
  return async function create(options) {
    const config = snapshotRuntime(options, platform);
    const catalog = catalogIndex(await platform.loadCatalog({ module: config.module }));
    const defaults = resolveDefaults(config, catalog);
    return createRuntime(config, catalog, defaults, platform);
  };
}

function snapshotRuntime(options, platform) {
  if (!options || typeof options !== "object" || Array.isArray(options)) {
    throw new TypeError("Nanocodex.create requires options");
  }
  for (const key of Object.keys(options)) {
    if (Object.hasOwn(RUNTIME_REJECTED, key)) throw new TypeError("Nanocodex.create does not accept " + key + ": " + RUNTIME_REJECTED[key]);
    if (!RUNTIME_KEYS.has(key)) throw new TypeError("unsupported Nanocodex.create option: " + key);
  }
  const input = options.backends;
  if (!input || typeof input !== "object" || Array.isArray(input)) {
    throw new TypeError("Nanocodex.create requires backends: { codex?, claude? }");
  }
  for (const key of Object.keys(input)) {
    if (!FAMILIES.includes(key)) throw new TypeError("unsupported backend family: " + key);
  }
  const backends = {};
  if (input.codex !== undefined) backends.codex = snapshotCodex(input.codex);
  if (input.claude !== undefined) backends.claude = snapshotClaude(input.claude);
  if (!Object.keys(backends).length) throw new TypeError("Nanocodex.create requires at least one backend");
  for (const key of ["instructions", "workspace"]) {
    if (options[key] !== undefined && typeof options[key] !== "string") throw new TypeError(key + " must be a string");
  }
  if (options.codeEvaluator !== undefined && typeof options.codeEvaluator !== "function") {
    throw new TypeError("codeEvaluator must be a function");
  }
  const codeMode = Object.values(backends).some((backend) => backend.codeMode);
  if (codeMode && options.codeEvaluator === undefined && platform.requiresEvaluator?.()) {
    throw new TypeError("Code Mode in this " + platform.placement + " placement requires an explicit codeEvaluator, such as createQuickJsEvaluator()");
  }
  const workspace = options.workspace;
  for (const [family, backend] of Object.entries(backends)) {
    if (backend.filesystem && backend.workspace === undefined && workspace !== undefined && workspace !== backend.filesystem.root) {
      throw new TypeError("workspace must match backends." + family + ".filesystem.root");
    }
  }
  return Object.freeze({
    backends: Object.freeze(backends),
    harness: options.harness,
    model: options.model,
    thinking: options.thinking,
    instructions: options.instructions,
    workspace,
    codeEvaluator: options.codeEvaluator,
    module: options.module,
  });
}

function backendEntry(value, family, isTransport) {
  // Transport shorthand: backends.codex = Transport.openAi(...).
  if (isTransport(value)) return { transport: value };
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new TypeError("backends." + family + " must be a transport or { transport, ... }");
  }
  return value;
}

function snapshotShared(entry, family, keys) {
  for (const key of Object.keys(entry)) {
    if (!keys.has(key)) throw new TypeError("unsupported backends." + family + " option: " + key);
  }
  if (entry.codeMode !== undefined && typeof entry.codeMode !== "boolean") {
    throw new TypeError("backends." + family + ".codeMode must be a boolean");
  }
  if (entry.workspace !== undefined && typeof entry.workspace !== "string") {
    throw new TypeError("backends." + family + ".workspace is the model's working directory string; pass a Workspace adapter as backends." + family + ".filesystem");
  }
  const filesystem = entry.filesystem;
  if (filesystem !== undefined) {
    if (!filesystem || typeof filesystem !== "object" || typeof filesystem.root !== "string") {
      throw new TypeError("backends." + family + ".filesystem must be a Workspace adapter");
    }
    if (entry.workspace !== undefined && entry.workspace !== filesystem.root) {
      throw new TypeError("backends." + family + ".workspace must match backends." + family + ".filesystem.root");
    }
  }
  const extras = {};
  for (const [key, value] of Object.entries(entry)) {
    if (!SHARED_BACKEND_KEYS.includes(key) && key !== "filesystem" && value !== undefined) extras[key] = CAPABILITY_KEYS.has(key) ? value : plain(value);
  }
  return {
    transport: entry.transport,
    tools: snapshotTools(entry.tools, family),
    codeMode: entry.codeMode !== false,
    workspace: entry.workspace,
    filesystem,
    extras: Object.freeze(extras),
  };
}

function isResponsesTransport(value) {
  try { resolveResponsesTransport(value); return true; } catch { return false; }
}

function snapshotCodex(value) {
  if (value && managedTransportOptions(value) !== undefined) throw managedError();
  const entry = backendEntry(value, "codex", (candidate) => isResponsesTransport(candidate));
  if (managedTransportOptions(entry.transport) !== undefined) throw managedError();
  if (!isResponsesTransport(entry.transport)) {
    throw new TypeError("backends.codex.transport requires Transport.openAi, chatGpt, mpp or hostManaged");
  }
  return Object.freeze(snapshotShared(entry, "codex", CODEX_KEYS));
}

function snapshotClaude(value) {
  const entry = backendEntry(value, "claude", isClaudeTransport);
  if (!isClaudeTransport(entry.transport)) {
    throw new TypeError("backends.claude.transport requires Transport.anthropic()");
  }
  const backend = snapshotShared(entry, "claude", CLAUDE_KEYS);
  return Object.freeze({ ...backend, auth: claudeTransportOptions(entry.transport) });
}

function managedError() {
  return new TypeError("Transport.managed opens one server-owned durable Agent; use Agent.create({ transport: Transport.managed(...) }) instead of Nanocodex.create");
}

/**
 * Deep-copies caller-owned plain data, including inside shallow-frozen tool
 * definitions. Functions, class instances and empty frozen tokens (opaque
 * WeakMap-keyed capabilities) keep their identity.
 */
function plain(value) {
  if (Array.isArray(value)) return Object.freeze(value.map(plain));
  if (!value || typeof value !== "object") return value;
  const prototype = Object.getPrototypeOf(value);
  if (prototype !== Object.prototype && prototype !== null) return value;
  if (Object.isFrozen(value) && Reflect.ownKeys(value).length === 0) return value;
  const copy = {};
  for (const key of Reflect.ownKeys(value)) copy[key] = plain(value[key]);
  return Object.freeze(copy);
}

function snapshotTools(tools, family) {
  if (tools === undefined) return undefined;
  // A factory materializes a fresh tool set for every Agent host.
  if (typeof tools === "function") return tools;
  if (tools && typeof tools === "object" && !Array.isArray(tools)
    && (typeof tools.close === "function" || typeof tools.attach === "function")) {
    throw new TypeError("backends." + family + ".tools received a createTools() instance, which belongs to one Agent host; pass a factory such as () => createTools(...)");
  }
  if (Array.isArray(tools)) return plain(tools);
  if (family === "claude") throw new TypeError("backends.claude.tools must be an array of Claude tools or a factory");
  if (!tools || typeof tools !== "object") throw new TypeError("backends.codex.tools must be a tool array, tool map or factory");
  return plain(tools);
}

/** Declarative tools are copied; a factory is awaited and its closable result is rolled back on failure. */
async function materializeTools(tools, context, created) {
  if (typeof tools !== "function") return Array.isArray(tools) ? [...tools] : tools === undefined ? undefined : { ...tools };
  const value = await tools(Object.freeze({ ...context }));
  if (value && typeof value.close === "function") created.push(value);
  return value;
}

/** Closes factory-created tools; Tools.close() is idempotent when a claiming host already closed it. */
async function release(created) {
  const results = await Promise.allSettled(created.map((tools) => Promise.resolve().then(() => tools.close())));
  return results.filter((result) => result.status === "rejected").map((result) => result.reason);
}

function catalogIndex(entries) {
  if (!Array.isArray(entries)) throw new TypeError("the bundled WASM did not return a model catalog");
  const models = new Map();
  for (const entry of entries) {
    if (entry?.transport === "native" && typeof entry.model === "string") models.set(entry.model, entry);
  }
  return models;
}

function familyDefault(catalog, family) {
  for (const entry of catalog.values()) if (entry.family === family && entry.familyDefault === true) return entry;
  throw new Error("this WASM build does not report a default " + family + " model; select a model explicitly");
}

function capabilityError(message, capability) {
  const error = new Error(message);
  error.code = "unsupported_capability";
  error.capability = capability;
  return error;
}

function requireBackend(config, family) {
  const backend = config.backends[family];
  if (backend === undefined) {
    throw new Error("no " + family + " backend is configured; Nanocodex never falls back to another family's credentials");
  }
  return backend;
}

function checkFamily(value, label) {
  if (value !== undefined && !FAMILIES.includes(value)) throw new TypeError(label + " must be codex or claude");
}

function checkThinking(entry, thinking) {
  if (thinking === undefined) return;
  if (typeof thinking !== "string") throw new TypeError("thinking must be a string");
  if (entry && !entry.thinking.includes(thinking)) {
    throw new TypeError(entry.model + " does not accept thinking " + thinking + "; accepted: " + entry.thinking.join(", "));
  }
}

/** A model belongs to exactly one family; uncataloged ids are Claude endpoint models only. */
function modelEntry(config, catalog, model, harness) {
  if (typeof model !== "string" || !model.trim()) throw new TypeError("model must be a non-empty string");
  const entry = catalog.get(model);
  if (entry) {
    if (harness !== undefined && harness !== entry.family) {
      throw new TypeError("model " + model + " belongs to the " + entry.family + " harness, not " + harness);
    }
    return { family: entry.family, entry };
  }
  if (harness === "claude" && config.backends.claude?.auth.endpoint !== undefined) return { family: "claude", entry: undefined };
  throw new TypeError("unknown model " + model + (harness === "claude"
    ? "; an uncataloged Claude model requires an explicit Transport.anthropic endpoint"
    : "; use a cataloged model id or harness claude with an explicit endpoint"));
}

/**
 * Default family: explicit harness, else the explicit model's family, else
 * codex when configured, else the only backend. Default model: explicit, else
 * that family's catalog default. Thinking: explicit, else the model's default.
 */
function resolveDefaults(config, catalog) {
  checkFamily(config.harness, "harness");
  let harness = config.harness;
  let entry;
  let model = config.model;
  if (model !== undefined) {
    const resolved = modelEntry(config, catalog, model, harness);
    harness ??= resolved.family;
    entry = resolved.entry;
  }
  harness ??= config.backends.codex !== undefined ? "codex" : Object.keys(config.backends)[0];
  requireBackend(config, harness);
  if (model === undefined) {
    entry = familyDefault(catalog, harness);
    model = entry.model;
  }
  checkThinking(entry, config.thinking);
  return Object.freeze({ harness, model, thinking: config.thinking ?? entry?.defaultThinking });
}

/** Selection against the runtime defaults, matching native subagent spawn semantics. */
function select(config, catalog, defaults, { harness, model, thinking }) {
  checkFamily(harness, "harness");
  let family, entry, resolved;
  if (model !== undefined) {
    ({ family, entry } = modelEntry(config, catalog, model, harness));
    resolved = model;
  } else if (harness === undefined || harness === defaults.harness) {
    family = defaults.harness;
    resolved = defaults.model;
    entry = catalog.get(resolved);
  } else {
    family = harness;
    entry = familyDefault(catalog, family);
    resolved = entry.model;
  }
  requireBackend(config, family);
  checkThinking(entry, thinking);
  const inherited = family === defaults.harness && resolved === defaults.model;
  return Object.freeze({
    harness: family,
    model: resolved,
    thinking: thinking ?? (inherited ? defaults.thinking : entry?.defaultThinking),
  });
}

function childSelection(catalog, defaults, family) {
  if (family === defaults.harness) return { model: defaults.model, thinking: defaults.thinking };
  const entry = familyDefault(catalog, family);
  return { model: entry.model, thinking: entry.defaultThinking };
}

function toolMode(backend) {
  return backend.codeMode ? "code-only" : "direct";
}

/** spawn > backend > runtime; a filesystem adapter fixes the working directory to its root. */
function backendWorkspace(config, backend, override) {
  const root = backend.filesystem?.root;
  const workspace = override ?? backend.workspace ?? (root ?? config.workspace);
  if (root !== undefined && workspace !== root) {
    throw new TypeError("the workspace must match the backend filesystem root " + root);
  }
  return workspace;
}

/** The same rooted file tools Codex installs for filesystem, as explicit named tools. */
function withFilesystemTools(tools, filesystem, family) {
  if (filesystem === undefined) return tools;
  const named = Object.entries(workspaceTools(filesystem)).map(([name, tool]) => family === "claude"
    ? { name, description: tool.description, inputSchema: tool.parameters, handler: (input) => tool.handler(input) }
    : { name, ...tool });
  if (tools === undefined) return named;
  if (Array.isArray(tools)) return [...tools, ...named];
  if (family === "codex" && (typeof tools.close === "function" || typeof tools.attach === "function")) {
    throw new TypeError("a createTools() instance cannot be combined with backends.codex.filesystem in an alternate-family child; configure its workspace inside createTools");
  }
  if (family === "claude") throw new TypeError("Claude tools must be an array");
  return { ...tools, ...Object.fromEntries(named.map(({ name, ...tool }) => [name, tool])) };
}

/** Alternate-family children use their own backend's workspace, falling back to the root's. */
function childWorkspace(backend, rootWorkspace) {
  return backend.filesystem?.root ?? backend.workspace ?? rootWorkspace;
}

async function codexChild(config, catalog, defaults, rootWorkspace, created) {
  const backend = config.backends.codex;
  const { model, thinking } = childSelection(catalog, defaults, "codex");
  const workspace = childWorkspace(backend, rootWorkspace);
  const recipe = { transport: backend.transport, model, thinking, toolMode: toolMode(backend),
    tools: withFilesystemTools(await materializeTools(backend.tools, { harness: "codex", role: "child" }, created), backend.filesystem, "codex") };
  if (workspace !== undefined) recipe.workspace = workspace;
  if (config.codeEvaluator !== undefined) recipe.codeEvaluator = config.codeEvaluator;
  for (const key of CODEX_CHILD_KEYS) if (backend.extras[key] !== undefined) recipe[key] = backend.extras[key];
  return recipe;
}

async function claudeChild(config, catalog, defaults, rootWorkspace, created) {
  const backend = config.backends.claude;
  const { model, thinking } = childSelection(catalog, defaults, "claude");
  const workspace = childWorkspace(backend, rootWorkspace);
  const recipe = { ...backend.auth, model, toolMode: toolMode(backend),
    tools: withFilesystemTools(await materializeTools(backend.tools, { harness: "claude", role: "child" }, created), backend.filesystem, "claude") ?? [] };
  if (thinking !== undefined) recipe.thinking = thinking;
  if (workspace !== undefined) recipe.workspace = workspace;
  if (config.codeEvaluator !== undefined) recipe.codeEvaluator = config.codeEvaluator;
  for (const [key, value] of Object.entries(backend.extras)) if (!CLAUDE_CHILD_EXCLUDED.has(key)) recipe[key] = value;
  return recipe;
}

async function agentOptions(config, catalog, defaults, family, settings, created) {
  const backend = config.backends[family];
  const instructions = settings.instructions ?? config.instructions;
  const shared = {};
  if (config.codeEvaluator !== undefined) shared.codeEvaluator = config.codeEvaluator;
  if (config.module !== undefined) shared.module = config.module;
  if (instructions !== undefined) shared.instructions = instructions;
  if (settings.resume !== undefined) {
    shared.resume = settings.resume;
    // Without the checkpoint's own ID, a resume would start a new root continuing it.
    if (typeof settings.resume.session_id === "string") shared.sessionId = settings.resume.session_id;
  }
  if (family === "codex") {
    const workspace = backendWorkspace(config, backend, settings.workspace);
    const options = { ...backend.extras, ...shared, transport: backend.transport, toolMode: toolMode(backend),
      tools: await materializeTools(backend.tools, { harness: "codex", role: "root" }, created) };
    if (settings.model !== undefined) options.model = settings.model;
    if (settings.thinking !== undefined) options.thinking = settings.thinking;
    if (workspace !== undefined) options.workspace = workspace;
    if (backend.filesystem !== undefined) options.filesystem = backend.filesystem;
    if (config.backends.claude !== undefined) options.harnesses = { claude: await claudeChild(config, catalog, defaults, workspace, created) };
    return options;
  }
  const workspace = backendWorkspace(config, backend, settings.workspace);
  const options = { ...backend.extras, ...backend.auth, ...shared, harness: "claude", model: settings.model,
    toolMode: toolMode(backend), subagents: backend.extras.subagents ?? {},
    tools: withFilesystemTools(await materializeTools(backend.tools, { harness: "claude", role: "root" }, created), backend.filesystem, "claude") ?? [] };
  if (settings.thinking !== undefined) options.thinking = settings.thinking;
  if (workspace !== undefined) options.workspace = workspace;
  if (config.backends.codex !== undefined) options.harnesses = { codex: await codexChild(config, catalog, defaults, workspace, created) };
  return options;
}

function checkKeys(value, keys, label) {
  if (value === undefined) return {};
  if (!value || typeof value !== "object" || Array.isArray(value)) throw new TypeError(label + " options must be an object");
  for (const key of Object.keys(value)) if (!keys.has(key)) throw new TypeError("unsupported " + label + " option: " + key);
  for (const key of ["instructions", "workspace"]) {
    if (value[key] !== undefined && typeof value[key] !== "string") throw new TypeError(key + " must be a string");
  }
  return value;
}

function closedError() {
  return new Error("the Nanocodex runtime has shut down");
}

function createRuntime(config, catalog, defaults, platform) {
  const owned = new Set();
  const admissions = new Set();
  let closing;

  for (const backend of Object.values(config.backends)) {
    if (!backend.codeMode && platform.supportsDirectTools?.() === false) {
      throw capabilityError("direct tools (codeMode: false) are unavailable in the " + platform.placement + " placement", "directTools");
    }
  }

  const owner = Object.freeze({ admit: (start) => admit(start) });

  /** One admission: closed gate, then construction, then ownership; late admissions are shut down. */
  function admit(start) {
    if (closing) return Promise.reject(closedError());
    const admission = (async () => {
      const agent = await start();
      own(agent);
      if (closing) {
        await shutdownOwnedAgent(agent).catch(() => {});
        throw closedError();
      }
      return agent;
    })();
    admissions.add(admission);
    void admission.then(() => admissions.delete(admission), () => admissions.delete(admission));
    return admission;
  }

  /** Built inside the admission so tool factories never run after shutdown begins. */
  function construct(family, settings) {
    return admit(async () => {
      const created = [];
      try {
        return await platform.createAgent(await agentOptions(config, catalog, defaults, family, settings, created));
      } catch (error) {
        // Awaited so shutdown never resolves while a failed admission is still closing its tools.
        const cleanup = await release(created);
        if (cleanup.length) throw new AggregateError([error, ...cleanup], "Nanocodex agent creation failed and its tools did not close");
        throw error;
      }
    });
  }

  function own(agent) {
    // Forks and session siblings, through agent.session or Actions, are admitted by this owner.
    retainAgentForOwner(agent, owner);
    owned.add(agent);
    observeAgentRelease(agent, () => owned.delete(agent));
  }

  function spawn(options) {
    let selection, settings;
    try {
      settings = checkKeys(options, SPAWN_KEYS, "nano.spawn");
      selection = select(config, catalog, defaults, settings);
    } catch (error) { return Promise.reject(error); }
    return construct(selection.harness, { ...selection, instructions: settings.instructions, workspace: settings.workspace });
  }

  function resume(checkpoint, options) {
    let family, settings;
    try {
      settings = checkKeys(options, RESUME_KEYS, "nano.resume");
      if (!checkpoint || typeof checkpoint !== "object" || Array.isArray(checkpoint)
        || typeof checkpoint.format !== "string" || typeof checkpoint.model !== "string") {
        throw new TypeError("nano.resume requires a SessionCheckpoint");
      }
      checkFamily(settings.harness, "harness");
      const cataloged = catalog.get(checkpoint.model)?.family;
      if (cataloged !== undefined && settings.harness !== undefined && settings.harness !== cataloged) {
        throw new TypeError("this checkpoint belongs to the " + cataloged + " harness, not " + settings.harness);
      }
      family = cataloged ?? settings.harness;
      if (family === undefined) {
        throw new TypeError("checkpoint model " + checkpoint.model + " is not cataloged; pass { harness } to resume it");
      }
      requireBackend(config, family);
    } catch (error) { return Promise.reject(error); }
    // The checkpoint keeps its own session identity, model and thinking.
    return construct(family, { model: family === "claude" ? checkpoint.model : undefined,
      instructions: settings.instructions, workspace: settings.workspace, resume: checkpoint });
  }

  function shutdown() {
    return closing ??= (async () => {
      await Promise.allSettled([...admissions]);
      const results = await Promise.allSettled([...owned].map((agent) => shutdownOwnedAgent(agent)));
      const errors = results.filter((result) => result.status === "rejected").map((result) => result.reason);
      if (errors.length) throw new AggregateError(errors, "one or more Nanocodex agents failed to shut down");
    })();
  }

  return Object.freeze({
    placement: platform.placement,
    defaults,
    backends: Object.freeze(Object.keys(config.backends)),
    spawn,
    resume,
    shutdown,
  });
}
