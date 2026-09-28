import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { Provider, secp256k1, Storage } from "accounts";
import { custom, decodeFunctionData, parseAbi } from "viem";
import { Transaction, Abis } from "viem/tempo";
import { tempo } from "viem/tempo/chains";
import { Challenge, Credential } from "mppx";
import { executeMercatorPayment, MercatorPaymentInputError } from "../src/mercator-payment.ts";
// Protocol failures: quote drift, token/chain/recipient escalation, redirects,
// duplicate/concurrent submission, and lost response after signing. The positive
// scenario uses the actual Accounts signer; only merchant/RPC transport is fake.
const payee = "0x0000000000000000000000000000000000000002";
const input = { idempotency_key: "synthetic-lookup-1", approved_total: "0.05", plan: { nodes: [{ id: "one", serviceId: "synthetic", method: "GET", path: "/lookup", input: {} }] } };
function fixture(patch = {}, loseResponse = false, challengePatch = {}) {
    const rows = new Map();
    const store = { async get(key) { return rows.get(key); }, async put(key, value) { rows.set(key, value); } };
    const wallet = Provider.create({ adapter: secp256k1({ privateKey: `0x${"12".repeat(32)}` }), storage: Storage.memory(), chains: [tempo], mpp: false,
        transports: { [tempo.id]: custom({ async request({ method, params }) {
                    if (method === "eth_call" && params?.[0]?.calls) return "0x";
                    if (method === "eth_chainId")
                        return "0x1079";
                    if (method === "eth_call") {
                        if (params?.[0]?.data?.startsWith("0x70a08231")) return `0x${"0".repeat(58)}ffffff`;
                        if (params?.[0]?.data === "0x313ce567") return `0x${"0".repeat(63)}6`;
                        return `0x${"0".repeat(64)}`;
                    }
                    if (method === "eth_estimateGas")
                        return "0x186a0";
                    if (method === "eth_gasPrice" || method === "eth_maxPriorityFeePerGas")
                        return "0x1";
                    if (method === "eth_getTransactionCount")
                        return "0x0";
                    if (method === "eth_getBlockByNumber")
                        return { number: "0x1", timestamp: "0x1", baseFeePerGas: "0x1", gasLimit: "0x1000000", gasUsed: "0x0" };
                    throw new Error(`Unexpected RPC ${method}`);
                } }) } });
    let submissions = 0;
    let credential;
    const fetcher = async (_url, init) => {
        assert.deepEqual(JSON.parse(String(init?.body)), { idempotencyKey: input.idempotency_key, plan: input.plan });
        const headers = new Headers(init?.headers);
        if (headers.has("authorization")) {
            submissions++;
            credential = Credential.deserialize(headers.get("authorization"));
            if (loseResponse)
                throw new Error("lost response");
            return Response.json({ id: "synthetic-job", status: "pending" }, { status: 201 });
        }
        return new Response(null, { status: 402, headers: { "www-authenticate": Challenge.serialize(Challenge.from({
                    id: "synthetic-challenge", realm: "mercator.sh", method: "tempo", intent: "charge",
                    expires: new Date(Date.now() + 60_000).toISOString(), ...challengePatch,
                    request: { amount: "50000", currency: "0x20c000000000000000000000b9537d11c60e8b50", recipient: payee, methodDetails: { chainId: 4217, feePayer: true, supportedModes: ["pull"], machineTokenEnabled: true }, ...patch },
                })) } });
    };
    return { store, wallet, fetcher, submissions: () => submissions, credential: () => credential };
}
describe("bounded account Mercator payments", () => {
    it("signs a sponsored MACH charge with the account wallet and replays without paying twice", async () => {
    const f = fixture();
        const result = await executeMercatorPayment(input, f);
        assert.equal(result.status, "submitted"); assert.equal(result.result.id, "synthetic-job");
        assert.equal(f.credential()?.payload.type,"transaction"); assert.match(f.credential().payload.signature,/^0x/);
        const payload = f.credential().payload;
        const transaction = Transaction.deserialize(payload.signature);
        assert.equal(transaction.chainId, 4217);
        assert.equal(transaction.feePayerSignature, null);
        assert.equal((transaction.calls).length, 2);
        assert.equal(transaction.calls[0].to?.toLowerCase(), "0x20c000000000000000000000f37de3740adec032");
        const approval = decodeFunctionData({ abi: Abis.tip20, data: transaction.calls[0].data });
        assert.equal(approval.functionName,"approve"); assert.equal(approval.args[1],50000n);
        const settlement = decodeFunctionData({ abi: parseAbi(["function swapTo(address inputToken,uint256 amount,address targetToken,address recipient,bytes32 memo)"]), data: transaction.calls[1].data });
        assert.equal(settlement.functionName, "swapTo");
        assert.deepEqual(settlement.args.slice(1, 4).map(v => typeof v === "string" ? v.toLowerCase() : v), [50000n, "0x20c000000000000000000000b9537d11c60e8b50", payee]);
        assert.equal(f.credential()?.challenge.request.currency, "0x20c000000000000000000000b9537d11c60e8b50");
        assert.deepEqual(await executeMercatorPayment(input, f), result);
        assert.equal(f.submissions(), 1);
        await assert.rejects(executeMercatorPayment({ ...input, approved_total: "0.04" }, f), /conflict/);
    });
    it("refuses a USDC fallback when MACH cannot fund the canonical route", async () => {
        const f = fixture();
        f.wallet.getMppxParameters = (() => {
            const original = f.wallet.getMppxParameters.bind(f.wallet);
            return () => {
                const params = original();
                return { ...params, async getClient(info) {
                        const client = await params.getClient(info);
                        return { ...client, request: async (...args) => {
                                const req = args[0];
                                if (req.method === "eth_call" && req.params?.[0]?.to?.toLowerCase() === "0x20c000000000000000000000f37de3740adec032")
                                    return `0x${"0".repeat(64)}`;
                                return client.request(...args);
                            } };
                    } };
            };
        })();
        const result = await executeMercatorPayment(input, f);
        assert.equal(result.status, "rejected");
        assert.equal(f.submissions(), 0);
    });
    it("does not reserve a key when an already-canceled queued request begins", async () => {
        const f = fixture();
        const controller = new AbortController(); controller.abort();
        await assert.rejects(executeMercatorPayment(input, { ...f, signal: controller.signal }));
        assert.equal(await f.store.get(`mercator-payment:${input.idempotency_key}`), undefined);
        assert.equal(f.submissions(), 0);
    });
    it("retains the reservation if result persistence fails after merchant acceptance", async () => {
        const f = fixture();
        const put = f.store.put;
        let writes = 0;
        f.store.put = async (key, value) => { if (++writes === 2)
            throw new Error("storage interrupted"); return put(key, value); };
        await assert.rejects(executeMercatorPayment(input, f), /storage interrupted/);
        assert.equal((await executeMercatorPayment(input, f)).status, "unknown");
        assert.equal(f.submissions(), 1);
    });
    it("rejects session, stale or invalid expiry and redirects without payment", async () => {
        for (const patch of [{ intent: "session" }, { expires: "not-a-date" }, { expires: new Date(0).toISOString() }, { realm: "foreign.example" }]) {
            const f = fixture({}, false, patch);
            assert.equal((await executeMercatorPayment(input, f)).status, "rejected");
            assert.equal(f.submissions(), 0);
        }
        const f = fixture();
        f.fetcher = async () => new Response(null, { status: 302, headers: { location: "https://foreign.example" } });
        assert.equal((await executeMercatorPayment(input, f)).status, "rejected");
    });
    it("cancels a stalled accepted response body and retains unknown", async () => {
        const f = fixture();
        const controller = new AbortController();
        const fetcher = f.fetcher;
        let cancelled = false;
        f.fetcher = async (url, init) => {
            const response = await fetcher(url, init);
            if (response.status !== 201)
                return response;
            queueMicrotask(() => controller.abort());
            return new Response(new ReadableStream({ cancel() { cancelled = true; } }), { status: 201 });
        };
        assert.equal((await executeMercatorPayment(input, { ...f, signal: controller.signal })).status, "unknown");
        assert.equal(cancelled, true);
        assert.equal((await executeMercatorPayment(input, f)).status, "unknown");
        assert.equal(f.submissions(), 1);
    });
    it("retains unknown outcomes without signing again", async () => {
        const f = fixture({}, true);
        assert.equal((await executeMercatorPayment(input, f)).status, "unknown");
        assert.equal((await executeMercatorPayment(input, f)).status, "unknown");
        assert.equal(f.submissions(), 1);
    });
    it("fails closed on changed price, token, recipient, chain, and excessive approval", async () => {
        for (const patch of [{ amount: "50001" }, { currency: payee }, { recipient: "bad" }, { methodDetails: { chainId: 1, feePayer: true } }]) {
            const f = fixture(patch);
            assert.equal((await executeMercatorPayment(input, f)).status, "rejected");
            assert.equal(f.submissions(), 0);
        }
        await assert.rejects(executeMercatorPayment({ ...input, approved_total: "0.050001" }, fixture()),
            error => error instanceof MercatorPaymentInputError && error.status === 400);
    });
});
