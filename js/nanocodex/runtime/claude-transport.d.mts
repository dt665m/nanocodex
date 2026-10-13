declare const claudeTransport: unique symbol;

/** Opaque Claude Messages authentication for Nanocodex.create({ backends: { claude } }). */
export type ClaudeTransport = Readonly<{ [claudeTransport]: true }>;


type ClaudeEndpointOptions = Readonly<{
  /** Absolute Messages URL. Defaults to the Anthropic API, or the subscription Messages URL for a subscription. */
  endpoint?: string | undefined;
  /** Host Messages fetch; never serialized into model or session state. */
  fetch?: typeof globalThis.fetch | undefined;
}>;

/** Exactly one explicit credential source. Nothing is discovered from the environment. */
export function anthropic(options: ClaudeEndpointOptions & (
  | Readonly<{ apiKey: string; headers?: never; subscription?: never; subscriptionIdentity?: never }>
  | Readonly<{ headers(): HeadersInit | Promise<HeadersInit>; apiKey?: never; subscription?: never; subscriptionIdentity?: never }>
  | Readonly<{
    /** An open handle from ClaudeSubscription.open() (nanocodex/worker). Each request reads the subscription's current private headers; keep it open until the runtime shuts down. */
    subscription: import("../worker/ClaudeSubscription.mjs").Subscription;
    subscriptionIdentity?: import("./claude.mjs").Options["subscriptionIdentity"];
    apiKey?: never;
    headers?: never;
  }>
)): ClaudeTransport;
