import { DurableObject } from "cloudflare:workers";
import { createManagedBrowserRuntime, type ManagedBrowserEnv } from "../src/browser-runtime";
import { SAGE_STRIPE_LOAD_RECOVERY_EXPRESSION } from "../src/kitesurf-sage-recovery";
import type { ToolContext } from "nanocodex";

interface Env extends ManagedBrowserEnv { SMOKE: DurableObjectNamespace<SageRecoverySmoke> }

export class SageRecoverySmoke extends DurableObject<Env> {
  async fetch(request: Request): Promise<Response> {
    const { checkoutUrl } = await request.json<{ checkoutUrl: string }>();
    const url = new URL(checkoutUrl);
    if (url.origin !== "https://app.arketa.co" ||
      !/^\/iframe\/sagepilates\/(calendar|schedule)\/checkout\/[^/]+\/?$/.test(url.pathname) || url.search || url.hash || url.username || url.password) {
      return Response.json({ error: "Supply a public Sage calendar/schedule checkout URL without query or fragment" }, { status: 400 });
    }
    const runtime = await createManagedBrowserRuntime({ ctx: this.ctx, env: this.env, sessionId: "sage-recovery-smoke" });
    try {
      const tool = runtime.tools.find(tool => tool.name === "browser_execute")!;
      const execution = await tool.handler({ code: `
        const created = await cdp.send({ method: "Target.createTarget", params: { url: "about:blank" } });
        const attached = await cdp.attachToTarget({ targetId: created.targetId });
        const sessionId = typeof attached === "string" ? attached : attached.sessionId;
        await cdp.send({ method: "Page.enable", sessionId });
        async function evaluate(expression) {
          const response = await cdp.send({ method: "Runtime.evaluate", params: { expression, returnByValue: true }, sessionId });
          if (response.exceptionDetails) throw new Error("Page evaluation failed");
          return response.result.value;
        }
        const recipe = ${JSON.stringify(SAGE_STRIPE_LOAD_RECOVERY_EXPRESSION)};
        const scopeRejection = await evaluate(recipe);
        await cdp.send({ method: "Page.navigate", params: { url: ${JSON.stringify(checkoutUrl)} }, sessionId });
        await new Promise(resolve => setTimeout(resolve, 6000));
        const baseline = await evaluate('({ body: document.body.innerText.trim().slice(0, 6000), stripeType: typeof window.Stripe })');
        const recovery = await evaluate(recipe);
        await new Promise(resolve => setTimeout(resolve, 7000));
        const recovered = await evaluate('({ body: document.body.innerText.trim().slice(0, 6000) })');
        const secondAttempt = await evaluate(recipe);
        return { scopeRejection, baseline, recovery, recovered, secondAttempt };
      ` }, { callId: "sage-recovery-smoke", signal: AbortSignal.timeout(90_000) } as ToolContext) as {
        status?: string;
        result?: {
          scopeRejection: { status: string }; baseline: { body: string; stripeType: string };
          recovery: { status: string }; recovered: { body: string }; secondAttempt: { status: string };
        };
      };
      const result = execution.result;
      const checks = {
        completed: execution.status === "completed",
        scopeRejected: result?.scopeRejection.status === "out-of-scope",
        reproduced: result?.baseline.body === "Login" && result.baseline.stripeType === "function",
        dispatched: result?.recovery.status === "dispatched",
        checkoutVisible: !!result && /Login/i.test(result.recovered.body) && /\$\s*\d/.test(result.recovered.body)
          && /class|package|credit/i.test(result.recovered.body),
        duplicatePrevented: result?.secondAttempt.status === "already-attempted",
      };
      const passed = Object.values(checks).every(Boolean);
      // Public rendered text only; never return the runtime's raw trace or connection metadata.
      return Response.json({ passed, provider: runtime.provider, checks, result }, { status: passed ? 200 : 500 });
    } finally { await runtime.close(); }
  }
}

export default { fetch(request: Request, env: Env) {
  if (request.method !== "POST" || new URL(request.url).pathname !== "/smoke") return new Response("Use POST /smoke", { status: 404 });
  return env.SMOKE.getByName("sage-recovery").fetch(request);
} };
export { CodemodeRuntime } from "@cloudflare/codemode";
