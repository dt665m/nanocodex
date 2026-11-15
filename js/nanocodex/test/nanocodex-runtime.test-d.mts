import {
  ChatGptSubscription,
  Nanocodex,
  Transport,
  Workspace,
  createMemoryChatGptSubscriptionStore,
  type SessionCheckpoint,
} from "../node/index.mjs";
import { createTools, type DefaultAgent } from "../index.mjs";
import { Nanocodex as HostNanocodex, Transport as HostTransport, createQuickJsEvaluator } from "../host/index.mjs";
import { Nanocodex as BrowserNanocodex, Transport as BrowserTransport } from "../browser/index.mjs";
import { ClaudeSubscription } from "../worker/index.mjs";

declare const module: WebAssembly.Module;
declare const checkpoint: SessionCheckpoint;
declare const evaluator: Parameters<typeof createQuickJsEvaluator>[0];

const subscription = await ChatGptSubscription.open({ id: "types", store: createMemoryChatGptSubscriptionStore("types") });
const claudeSubscription = await ClaudeSubscription.open({ id: "types", module, store: {
  async load() { return { revision: "0" }; },
  async compareAndSwap() { return { status: "committed", revision: "1" } as const; },
} });
const filesystem = await Workspace.open({ path: "/tmp/nanocodex-types" });

const nano: Nanocodex.Runtime = await Nanocodex.create({
  backends: {
    codex: { transport: Transport.chatGpt({ subscription }), tools: () => createTools(), codeMode: true, filesystem },
    claude: { transport: Transport.anthropic({ apiKey: "synthetic" }), tools: [], workspace: "/claude", maxTokens: 1024 },
  },
  harness: "codex",
  model: "gpt-6.1-sol",
  thinking: "low",
  instructions: "shared",
});
const root: DefaultAgent = await nano.spawn({ instructions: "x", harness: "claude", model: "claude-sonnet-5-5", thinking: "high" });
await root.turn.prompt({ input: "hello" }).result();
const resumed: DefaultAgent = await nano.resume(checkpoint, { workspace: "/w" });
void resumed;
const harness: "codex" | "claude" = nano.defaults.harness;
void harness;
await nano.shutdown();

await Nanocodex.create({ backends: { codex: Transport.openAi({ apiKey: "synthetic" }) } });
await Nanocodex.create({ backends: { codex: Transport.openAi({ apiKey: "synthetic" }), claude: Transport.anthropic({ apiKey: "k" }) }, model: "claude-opus-5-5" });
// @ts-expect-error Claude backends take Claude tool arrays, not createTools() runtimes.
await Nanocodex.create({ backends: { claude: { transport: Transport.anthropic({ apiKey: "k" }), tools: () => createTools() } } });
await Nanocodex.create({ backends: { claude: Transport.anthropic({ headers: async () => ({ authorization: "Bearer synthetic" }) }) }, model: "claude-opus-5-5" });
await Nanocodex.create({ backends: { claude: Transport.anthropic({ subscription: claudeSubscription }) }, model: "claude-opus-5-5" });
await Nanocodex.create({ backends: { claude: { transport: Transport.anthropic({ apiKey: "k" }), tools: ({ role }) => role === "root" ? [] : [] } }, model: "claude-opus-5-5" });

await HostNanocodex.create({ backends: { codex: HostTransport.hostManaged(), claude: HostTransport.anthropic({ apiKey: "k" }) }, module, codeEvaluator: createQuickJsEvaluator(evaluator), model: "gpt-6.1-sol" });
await BrowserNanocodex.create({ backends: { codex: BrowserTransport.hostManaged() }, model: "gpt-6.1-sol" });

// @ts-expect-error tools belong to each backend.
await Nanocodex.create({ backends: { codex: Transport.openAi({ apiKey: "k" }) }, model: "gpt-6.1-sol", tools: [] });
// @ts-expect-error exactly one Claude credential source.
Transport.anthropic({ apiKey: "k", headers: async () => ({}) });
// @ts-expect-error Claude backends require Transport.anthropic.
await Nanocodex.create({ backends: { claude: Transport.openAi({ apiKey: "k" }) }, model: "gpt-6.1-sol" });
// @ts-expect-error per-root sessions are not configurable at runtime level.
await nano.spawn({ sessionId: "fixed" });
