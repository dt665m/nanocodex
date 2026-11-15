import { registerDefinitionHost, releaseDefinitionHost, releaseHostSessions, toWasmConfig } from '../internal.mjs';
import { createClaudeHost } from './claude-host.mjs';
import { toClaudeConfig } from './claude.mjs';
import { createBrowserHost } from '../browser/host.mjs';
import { resolveResponsesTransport } from './responses-transport.mjs';
import { resolveTools } from './tool-configuration.mjs';
import { rawSubscription } from './chatgpt-subscription.mjs';

const MPP_RESPONSES_URL = 'wss://openai.mpp.tempo.xyz/v1/responses';

/** Explicit alternate-family capabilities; the Rust registry owns every child. */
export async function prepareHarnesses(harnesses, emit, {
  createCodexHost = createBrowserHost,
  subagentSessions, subagentRouting, toolProviders, codeEffectJournal, traceTool, codeEvaluator,
  cloudflareReservation,
} = {}) {
  if (harnesses === undefined) return { close() {} };
  if (!harnesses || typeof harnesses !== 'object' || Array.isArray(harnesses)
    || Object.keys(harnesses).some(key => !['codex', 'claude'].includes(key))) throw new TypeError('harnesses accepts codex and claude capabilities');
  const hosts = [];
  const close = async () => {
    await Promise.all(hosts.map(async ([id, host]) => {
      releaseHostSessions(host);
      releaseDefinitionHost(id);
      await host.dispose();
    }));
  };
  const result = { close };
  try {
    if (harnesses.claude) {
      const options = { ...harnesses.claude, codeEvaluator: harnesses.claude.codeEvaluator ?? codeEvaluator };
      if (options.durability !== undefined || options.durabilityId !== undefined || options.sessionId !== undefined || options.harnesses !== undefined || options.resume !== undefined) throw new TypeError('child harness capabilities must be ephemeral and cannot contain nested harnesses');
      const config = toClaudeConfig(options);
      const host = createClaudeHost({ ...options, onEvent: emit, subagentSessions, subagentRouting, codeEffectJournal, traceTool });
      const id = registerDefinitionHost(host, cloudflareReservation);
      hosts.push([id, host]);
      const parallelSafeTools = host.parallelSafeTools();
      result.claude = { ...config, hostDefinitionId: id, authHostId: id, tools: JSON.parse(host.toolDefinitions()),
        ...(parallelSafeTools.length ? { parallelSafeTools } : {}) };
    }
    if (harnesses.codex) {
      const options = { ...harnesses.codex, codeEvaluator: harnesses.codex.codeEvaluator ?? codeEvaluator };
      if (options.durability !== undefined || options.durabilityId !== undefined || options.sessionId !== undefined || options.harnesses !== undefined || options.resume !== undefined) throw new TypeError('child harness capabilities must be ephemeral and cannot contain nested harnesses');
      const transport = resolveResponsesTransport(options.transport);
      // A ChatGPT subscription stays Rust-owned: the Claude root passes its raw
      // handle to createClaudeWithChatGpt, so Codex children refresh through
      // the same credential lifecycle. MPP frames flow through this host's mpp.ws.
      if (transport.subscription) result.codexSubscription = rawSubscription(transport.subscription);
      if (options.codeEvaluator !== undefined && typeof options.codeEvaluator !== 'function') throw new TypeError('Codex harness codeEvaluator must be a function');
      // Each recipe honors its own toolMode; only Code Mode needs an evaluator.
      if ((options.toolMode ?? 'code-only') === 'code-only' && createCodexHost === createBrowserHost && typeof globalThis.Worker !== 'function' && options.codeEvaluator === undefined) throw new TypeError('Codex harness Code Mode requires an explicit codeEvaluator outside a browser Worker host');
      const { tools } = resolveTools(options.tools, { defaultSubagents: false });
      const host = createCodexHost({ ...transport, hostAuth: transport.hostAuth === true, tools, workspace: options.workspace, toolMode: options.toolMode ?? 'code-only', codeEvaluator: options.codeEvaluator, onEvent: emit, subagentSessions, subagentRouting, toolProviders, codeEffectJournal, traceTool });
      const id = registerDefinitionHost(host, cloudflareReservation);
      hosts.push([id, host]);
      await host.ready();
      result.codex = toWasmConfig({ ...options, ...transport,
        apiKey: transport.apiKey ?? (transport.subscription ? 'subscription-managed' : transport.mpp ? 'mpp-managed' : 'host-managed'),
        websocketUrl: transport.websocketUrl ?? (transport.mpp ? MPP_RESPONSES_URL : undefined),
        hostDefinitionId: id });
    }
    return result;
  } catch (error) { await close(); throw error; }
}
