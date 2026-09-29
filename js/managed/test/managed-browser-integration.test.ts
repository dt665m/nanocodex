import { describe, expect, it, vi } from "vitest";
import type { ToolContext } from "nanocodex";
import type { ManagedBrowserEnv, ManagedBrowserRuntime } from "../src/browser-runtime";
import { ManagedSessionBrowser, turnCanUseExecutionNamespace } from "../src/index";

function fixture() {
  const data = new Map<string, unknown>();
  const ctx = { storage: {
    get: async (key: string) => data.get(key),
    put: async (key: string, value: unknown) => { data.set(key, value); },
    delete: async (key: string) => data.delete(key),
  } } as unknown as DurableObjectState;
  const env = { BROWSER: {}, LOADER: {}, MANAGED_BROWSER_PROVIDER: "kitesurf" } as ManagedBrowserEnv;
  const handler = vi.fn(async () => ({ title: "Example" }));
  const runtime = { provider: "kitesurf", tools: [
    { name: "browser_execute", description: "browse", parameters: {}, handler },
    { name: "browser_vault_fill", description: "private", parameters: {}, handler },
  ], expireAndSweep: vi.fn(async () => {}), close: vi.fn(async () => {}) } as unknown as ManagedBrowserRuntime;
  const create = vi.fn(async (_options: Parameters<typeof import("../src/browser-runtime").createManagedBrowserRuntime>[0]) => runtime);
  const schedule = vi.fn(async () => {});
  let authorization: Parameters<typeof turnCanUseExecutionNamespace>[0] = { capabilities: ["agents:write", "tools:use"] };
  const make = (id = "session-a") => new ManagedSessionBrowser(ctx, env, id,
    () => turnCanUseExecutionNamespace(authorization), schedule, create);
  const context = { signal: new AbortController().signal, sessionId: "session-a" } as ToolContext;
  return { data, env, runtime, handler, create, schedule, make, context, authorize: (value: typeof authorization) => { authorization = value; } };
}

describe("managed session hosted browser integration", () => {
  it("wires the session and binding, retains one runtime, and exposes only ordinary browsing", async () => {
    const f = fixture(), browser = f.make();
    const tools = await browser.tools();
    expect(tools.map(tool => tool.name)).toEqual(["browser_execute"]);
    await browser.tools();
    expect(f.create).toHaveBeenCalledTimes(1);
    expect(f.create.mock.calls[0]).toEqual([expect.objectContaining({ env: f.env, sessionId: "session-a" })]);
    expect(f.create.mock.calls[0]?.[0]).not.toHaveProperty("resolveVaultLogin");
    await expect(tools[0]!.handler({ code: 'await cdp.send({ method: "Target.getTargets" })' }, f.context)).resolves.toEqual({ title: "Example" });
    expect(f.schedule).toHaveBeenCalledTimes(2);
    expect(f.data.get(ManagedSessionBrowser.alarmKey)).toBeGreaterThan(Date.now());
  });

  it("checks current capabilities on every invocation and rejects Connect access to retained browsing", async () => {
    const f = fixture(), [tool] = await f.make().tools();
    for (const authorization of [undefined, { capabilities: [] }, { capabilities: ["tools:use"] },
      { capabilities: ["agents:write", "tools:use"], connectGrant: {} }] as Parameters<typeof turnCanUseExecutionNamespace>[0][]) {
      f.authorize(authorization);
      await expect(tool!.handler({}, f.context)).rejects.toThrow(/full account/);
    }
    expect(f.handler).not.toHaveBeenCalled();
    expect(f.schedule).not.toHaveBeenCalled();
  });

  it("reconstructs for a durable idle sweep and closes on deletion", async () => {
    const f = fixture();
    f.data.set(ManagedSessionBrowser.alarmKey, Date.now() - 1);
    const reconstructed = f.make();
    await reconstructed.sweep();
    expect(f.runtime.expireAndSweep).toHaveBeenCalledTimes(1);
    expect(f.data.has(ManagedSessionBrowser.alarmKey)).toBe(false);
    await reconstructed.close();
    expect(f.runtime.close).toHaveBeenCalledTimes(1);
  });

  it("keeps the cleanup deadline after tool failure or sweep failure", async () => {
    const f = fixture(), browser = f.make();
    f.handler.mockRejectedValueOnce(new Error("navigation failed"));
    const [tool] = await browser.tools();
    await expect(tool!.handler({}, f.context)).rejects.toThrow("navigation failed");
    expect(f.schedule).toHaveBeenCalledTimes(2);
    f.data.set(ManagedSessionBrowser.alarmKey, Date.now() - 1);
    vi.mocked(f.runtime.expireAndSweep).mockRejectedValueOnce(new Error("provider unavailable"));
    await expect(browser.sweep()).rejects.toThrow("provider unavailable");
    expect(f.data.get(ManagedSessionBrowser.alarmKey)).toBeGreaterThan(Date.now());
    expect(f.schedule).toHaveBeenCalledTimes(3);
  });

  it("does not construct hosted browser tools without deployment bindings", async () => {
    const f = fixture();
    delete f.env.BROWSER;
    expect(await f.make().tools()).toEqual([]);
    expect(f.create).not.toHaveBeenCalled();
  });
});
