import { SELF } from "cloudflare:test";
import { describe, expect, it } from "vitest";
const input = { idempotency_key: "synthetic-lookup-1", approved_total: "0.05", plan: { nodes: [{ id: "one", serviceId: "synthetic", method: "GET", path: "/lookup", input: {} }] } };
describe("broker Mercator execution", () => {
  it("executes through the broker with its encrypted wallet and serializes concurrent retries", async () => {
    const base = "https://broker.internal/users/mercator-synthetic-broker/wallet";
    const wallet = await SELF.fetch(base, { method: "PUT" });
    const metadata = await wallet.json<{ address: string }>();
    const send = () => SELF.fetch(`${base}/mercator`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(input) });
    const [first, replay] = await Promise.all([send(), send()]);
    const result = await first.json<{ result: { payer: string } }>();
    expect(result.result?.payer.toLowerCase()).toBe(`did:pkh:eip155:4217:${metadata.address}`);
    expect(result).toMatchObject({ status: "submitted", result: { id: "broker-synthetic-job", payer: expect.any(String), paidSubmissions: 1 } });
    expect(await replay.json()).toEqual(result);
    expect(JSON.stringify(result)).not.toMatch(/signature|privateKey|authorization/);
  });

});
