import { describe, expect, it } from "vitest";
import { mercatorPaymentTools } from "../src/mercator-tool";

describe("owner Mercator executor", () => {
  it("returns unknown when the caller disconnects and transport ignores cancellation", async () => {
    const controller = new AbortController();
    const tool = mercatorPaymentTools({ owner: "synthetic-owner", authorize() {}, broker: { fetch: async () => { controller.abort(); return await new Promise<Response>(() => {}); } } })[0]!;
    expect(await tool.handler!({}, { signal: controller.signal } as never)).toMatchObject({ status: "unknown" });
  });
  it("reports broker failures as unknown rather than encouraging a replacement spend", async () => {
    const tool = mercatorPaymentTools({ owner: "synthetic-owner", authorize() {}, broker: { fetch: async () => new Response(null, { status: 503 }) } as Pick<Fetcher, "fetch"> })[0]!;
    expect(await tool.handler!({}, { signal: new AbortController().signal } as never)).toMatchObject({ status: "unknown" });
  });
  it("denies restricted callers before contacting the broker", async () => {
    let calls = 0;
    const tools = mercatorPaymentTools({ owner: "synthetic-owner", broker: { fetch: async () => { calls++; return Response.json({}); } } as Pick<Fetcher, "fetch">,
      authorize() { throw new Error("forbidden"); } });
    await expect(tools[0]!.handler!({}, { signal: new AbortController().signal } as never)).rejects.toThrow("forbidden");
    expect(calls).toBe(0);
    expect(mercatorPaymentTools({ owner: "synthetic-owner", multiplayer: true, broker: {} as Pick<Fetcher, "fetch">, authorize() {} })).toEqual([]);
  });
});
