// Explicit Claude Messages authentication for Nanocodex.create backends.
// Credentials stay in this module's WeakMap; the token itself is an empty frozen object.
import { isClaudeSubscription } from "./claude-subscription.mjs";

const claudeTransports = new WeakMap();

/** Measured public subscription Messages endpoint (docs/claude-authentication.md). */
export const CLAUDE_SUBSCRIPTION_MESSAGES_URL = "https://api.anthropic.com/v1/messages?beta=true";

const KEYS = new Set(["apiKey", "headers", "subscription", "endpoint", "fetch", "subscriptionIdentity"]);

/**
 * Exactly one of an Anthropic Console apiKey, a caller-owned headers()
 * provider, or an open Rust-managed ClaudeSubscription handle.
 */
export function anthropic(options) {
  if (!options || typeof options !== "object" || Array.isArray(options)) {
    throw new TypeError("Transport.anthropic requires options");
  }
  for (const key of Object.keys(options)) {
    if (!KEYS.has(key)) throw new TypeError("unsupported Transport.anthropic option: " + key);
  }
  const sources = ["apiKey", "headers", "subscription"].filter((key) => options[key] !== undefined);
  if (sources.length !== 1) {
    throw new TypeError("Transport.anthropic requires exactly one of apiKey, headers or subscription");
  }
  if (options.fetch !== undefined && typeof options.fetch !== "function") {
    throw new TypeError("Transport.anthropic fetch must be a function");
  }
  if (options.endpoint !== undefined) endpoint(options.endpoint);
  if (options.subscriptionIdentity !== undefined && options.subscription === undefined) {
    throw new TypeError("subscriptionIdentity requires a Claude subscription");
  }
  const setup = { endpoint: options.endpoint, fetch: options.fetch };
  if (options.apiKey !== undefined) {
    if (typeof options.apiKey !== "string" || !options.apiKey.trim()) {
      throw new TypeError("Anthropic API key must be a non-empty string");
    }
    setup.auth = Object.freeze({ apiKey: options.apiKey });
  } else if (options.headers !== undefined) {
    if (typeof options.headers !== "function") {
      throw new TypeError("Transport.anthropic headers must be a function");
    }
    const headers = options.headers;
    setup.auth = Object.freeze({ headers: () => headers() });
  } else {
    const subscription = options.subscription;
    if (!isClaudeSubscription(subscription)) {
      throw new TypeError("Transport.anthropic subscription must be an open ClaudeSubscription");
    }
    const identity = options.subscriptionIdentity;
    if (identity !== undefined && (!identity || typeof identity !== "object" || Array.isArray(identity))) {
      throw new TypeError("subscriptionIdentity must be an object");
    }
    // The Rust lifecycle owns rotation; each outbound request reads its current private headers.
    setup.auth = Object.freeze({
      headers: async () => {
        const credential = await subscription.credential();
        const headers = credential?.headers;
        if (!headers || typeof headers !== "object" || Array.isArray(headers)) {
          throw new Error("Claude subscription returned no credential headers");
        }
        return headers;
      },
    });
    setup.compatibilityProfile = "subscription";
    setup.endpoint ??= CLAUDE_SUBSCRIPTION_MESSAGES_URL;
    if (identity !== undefined) setup.subscriptionIdentity = Object.freeze({ ...identity });
  }
  const transport = Object.freeze({});
  claudeTransports.set(transport, Object.freeze(setup));
  return transport;
}

export function isClaudeTransport(transport) {
  return claudeTransports.has(transport);
}

/** Claude Agent.create options for this transport; never serialized. */
export function claudeTransportOptions(transport) {
  const setup = claudeTransports.get(transport);
  if (setup === undefined) throw new TypeError("the claude backend requires Transport.anthropic()");
  const options = { auth: setup.auth };
  for (const key of ["endpoint", "fetch", "compatibilityProfile", "subscriptionIdentity"]) {
    if (setup[key] !== undefined) options[key] = setup[key];
  }
  return options;
}

function endpoint(value) {
  let url;
  try { url = new URL(value); } catch { url = undefined; }
  if (!url || !["https:", "http:"].includes(url.protocol) || url.username || url.password || url.hash) {
    throw new TypeError("Transport.anthropic endpoint must be an absolute HTTP URL without credentials or fragment");
  }
}
