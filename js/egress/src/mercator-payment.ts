import { decodeFunctionData, parseAbi } from "viem";
import { Abis } from "viem/tempo";
import { Challenge, Credential } from "mppx";
import { tempo as paymentTempo } from "mppx/client";
import type { Provider } from "accounts";

const ENDPOINT = "https://mercator.sh/v1/jobs";
const USDC = "0x20c000000000000000000000b9537d11c60e8b50";
const MACH = "0x20c000000000000000000000f37de3740adec032";
const SWAP = "0xf72e5107c32c655ffa7539a3c8e97b7c3ce16a3f";
const SWAP_ABI = parseAbi(["function swapTo(address inputToken,uint256 amount,address targetToken,address recipient,bytes32 memo)"]);
const ADDRESS = /^0x[0-9a-f]{40}$/i;
interface Store { get<T>(key: string): Promise<T | undefined>; put(key: string, value: unknown): Promise<unknown>; }
interface Options { store: Store; wallet: ReturnType<typeof Provider.create>; fetcher?: typeof fetch; signal?: AbortSignal; }
type Result = { status: "submitted" | "rejected" | "unknown"; operation_id: string; result?: unknown; reason?: string };
type RecordState = { fingerprint: string; result: Result };

/** Caller must serialize operations per account (the credential DO does so).
 * This endpoint is only reachable through the trusted owner service binding.
 * Explicit user authorization is enforced by the managed tool authority model.
 */
export async function executeMercatorPayment(value: unknown, { store, wallet, fetcher = fetch, signal: callerSignal }: Options): Promise<Result> {
  if (!record(value) || Object.keys(value).some(k => !["plan", "approved_total", "idempotency_key", "id"].includes(k))
    || !record(value.plan) || !Array.isArray(value.plan.nodes) || value.plan.nodes.length < 1 || value.plan.nodes.length > 10
    || typeof value.idempotency_key !== "string" || !/^[A-Za-z0-9_-]{8,200}$/.test(value.idempotency_key)
    || typeof value.approved_total !== "string" || !/^\d{1,2}(?:\.\d{1,6})?$/.test(value.approved_total)
    || (value.id !== undefined && (typeof value.id !== "string" || !/^[0-9a-f-]{36}$/i.test(value.id)))) throw new Error("Invalid Mercator payment request");
  const [whole, fraction = ""] = value.approved_total.split(".");
  const maximum = BigInt(whole!) * 1_000_000n + BigInt(fraction.padEnd(6, "0"));
  if (maximum <= 0n || maximum > 50_000n) throw new Error("Mercator approval must be positive and at most $0.05 per operation");
  const fingerprintBody = canonical(value);
  const body = canonical({ idempotencyKey: value.idempotency_key, plan: value.plan, ...(value.id === undefined ? {} : { id: value.id }) });
  if (new TextEncoder().encode(body).length > 64 * 1024) throw new Error("Mercator request exceeds 64 KiB");
  const fingerprint = await digest(fingerprintBody);
  const key = `mercator-payment:${value.idempotency_key}`;
  const previous = await store.get<RecordState>(key);
  if (previous) {
    if (previous.fingerprint !== fingerprint) throw new Error("Mercator idempotency key conflict");
    return previous.result;
  }
  const operation_id = value.idempotency_key;
  const unknown: Result = { status: "unknown", operation_id, reason: "Do not repay. This stored outcome requires manual merchant reconciliation using the idempotency key." };
  // Durable before any network activity: eviction, timeout and lost replies cannot
  // cause a second signature. Unknown outcomes intentionally require reconciliation.
  await store.put(key, { fingerprint, result: unknown });
  const signal = AbortSignal.any([AbortSignal.timeout(60_000), ...(callerSignal ? [callerSignal] : [])]);
  let paymentSubmitted = false;
  let result: Result;
  try {
    signal.throwIfAborted();
    const init: RequestInit = { method: "POST", redirect: "manual", signal, headers: { "content-type": "application/json", accept: "application/json", "accept-payment": "tempo/charge", "idempotency-key": operation_id }, body };
    const quote = await fetchResponse(fetcher, init, signal);
    if (quote.status !== 402) {
      if (!quote.ok) { void quote.body?.cancel().catch(() => {}); throw new Error("Merchant rejected request before payment"); }
      result = { status: "submitted", operation_id, result: await boundedJson(quote, signal) };
    } else {
      void quote.body?.cancel().catch(() => {});
      const header = quote.headers.get("www-authenticate");
      if (!header) throw new Error("No payment challenge");
      const challenge = Challenge.deserialize(header);
      const request = challenge.request as Record<string, unknown>;
      const details = request.methodDetails;
      if (challenge.method !== "tempo" || challenge.intent !== "charge" || challenge.realm !== "mercator.sh"
        || typeof challenge.expires !== "string" || !Number.isFinite(Date.parse(challenge.expires)) || Date.parse(challenge.expires) <= Date.now() || Date.parse(challenge.expires) > Date.now() + 10 * 60_000
        || typeof request.amount !== "string" || !/^\d{1,10}$/.test(request.amount) || BigInt(request.amount) !== maximum
        || typeof request.currency !== "string" || ![USDC, MACH].includes(request.currency.toLowerCase())
        || typeof request.recipient !== "string" || !ADDRESS.test(request.recipient) || /^0x0{40}$/i.test(request.recipient)
        || !record(details) || details.chainId !== 4217 || details.feePayer !== true
        || details.splits !== undefined || !Array.isArray(details.supportedModes) || !details.supportedModes.includes("pull")) throw new Error("Payment challenge violates approved price, chain, token, recipient or sponsored-charge policy");
      // Live HTTPS challenge pins the payee. The SDK constructs and simulates
      // its canonical MACH settlement route without altering the challenge.
      if (request.currency.toLowerCase() !== MACH && details.machineTokenEnabled !== true) throw new Error("Merchant does not accept account MACH credits");
      await abortable(wallet.request({ method: "wallet_connect", params: [{ chainId: "0x1079", capabilities: { method: "login" } }] } as never), signal);
      const currency = request.currency;
      const recipient = request.recipient;
      const parameters = wallet.getMppxParameters();
      const method = paymentTempo.charge({ ...parameters, mode: "pull", autoSwap: false, expectedChainId: 4217,
        async resolveAccount(info) {
          if (info.chainId !== 4217 || info.operation.kind !== "executeCalls") throw new Error("Unsupported settlement operation");
          validateSettlement(info.operation.calls, maximum, currency, recipient);
          return parameters.resolveAccount(info);
        },
      });
      const encoded = await abortable(method.createCredential({ challenge: challenge as never, context: {} }), signal);
      signal.throwIfAborted();
      const credential = Credential.deserialize(encoded);
      const authorization = Credential.serialize({ ...credential, challenge });
      const headers = new Headers(init.headers);
      headers.set("authorization", authorization);
      paymentSubmitted = true;
      const response = await fetchResponse(fetcher, { ...init, headers, signal }, signal);
      if (!response.ok) { void response.body?.cancel().catch(() => {}); throw new Error("Paid submission outcome requires reconciliation"); }
      result = { status: "submitted", operation_id, result: await boundedJson(response, signal) };
    }
  } catch {
    result = paymentSubmitted ? unknown : { status: "rejected", operation_id, reason: "Request could not be submitted; no payment credential was sent. Check the quote and wallet funding before a new operation." };
  }
  await store.put(key, { fingerprint, result });
  return result;
}
function record(value: unknown): value is Record<string, any> { return value !== null && typeof value === "object" && !Array.isArray(value); }
function canonical(value: any): string { return JSON.stringify(value, (_key, entry) => record(entry) ? Object.fromEntries(Object.keys(entry).sort().map(key => [key, entry[key]])) : entry); }
async function digest(value: string): Promise<string> { return Array.from(new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value))), b => b.toString(16).padStart(2, "0")).join(""); }
async function boundedJson(response: Response, signal: AbortSignal): Promise<unknown> {
  const reader = response.body?.getReader();
  if (!reader) return null;
  const chunks: Uint8Array[] = []; let size = 0;
  try { for (;;) { const { value, done } = await abortable(reader.read(), signal); if (done) break; size += value.length; if (size > 256 * 1024) throw new Error("Merchant response too large"); chunks.push(value); } }
  finally { void reader.cancel().catch(() => {}); }
  const bytes = new Uint8Array(size); let offset = 0; for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.length; }
  return JSON.parse(new TextDecoder().decode(bytes));
}

function validateSettlement(calls: unknown, maximum: bigint, currency: string, recipient: string): void {
  if (!Array.isArray(calls) || calls.length !== 2 || calls.some(c => !record(c) || (c.value !== undefined && BigInt(c.value) !== 0n))) throw new Error("Only canonical MACH settlement is authorized");
  const [approval, swap] = calls;
  if (approval.to?.toLowerCase() !== MACH || swap.to?.toLowerCase() !== SWAP) throw new Error("Unsupported MACH route");
  const a = decodeFunctionData({ abi: Abis.tip20, data: approval.data });
  const b = decodeFunctionData({ abi: SWAP_ABI, data: swap.data });
  if (a.functionName !== "approve" || String(a.args[0]).toLowerCase() !== SWAP || a.args[1] !== maximum
    || b.functionName !== "swapTo" || b.args[0].toLowerCase() !== MACH || b.args[1] !== maximum
    || b.args[2].toLowerCase() !== currency.toLowerCase() || b.args[3].toLowerCase() !== recipient.toLowerCase()) throw new Error("Settlement exceeds approved calls");
}

function abortable<T>(pending: Promise<T>, signal: AbortSignal): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const abort = () => reject(new Error("Mercator operation interrupted"));
    if (signal.aborted) { pending.catch(() => {}); abort(); return; }
    signal.addEventListener("abort", abort, { once: true });
    pending.then(resolve, reject).finally(() => signal.removeEventListener("abort", abort));
  });
}

async function fetchResponse(fetcher: typeof fetch, init: RequestInit, signal: AbortSignal): Promise<Response> {
  const pending = fetcher(ENDPOINT, init);
  // A transport may resolve after cancellation; release that abandoned body too.
  void pending.then(response => { if (signal.aborted) void response.body?.cancel().catch(() => {}); }, () => {});
  return abortable(pending, signal);
}
