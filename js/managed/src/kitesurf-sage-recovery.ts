// Native CDP recipe text, shared by agent guidance and the real Browser Run fixture.
// This is never run implicitly by the browser runtime or injected into other pages.
export const SAGE_STRIPE_LOAD_RECOVERY_EXPRESSION = String.raw`(() => {
  if (location.origin !== "https://app.arketa.co" ||
      !/^\/iframe\/sagepilates\/(calendar|schedule)\/checkout\/[^/]+\/?$/.test(location.pathname)) {
    return { status: "out-of-scope" };
  }
  if (document.__nanocodexSageStripeLoadRecovered) return { status: "already-attempted" };
  if (performance.now() < 6000 || document.body?.innerText.trim() !== "Login") {
    return { status: "not-stalled" };
  }
  if (typeof window.Stripe !== "function") return { status: "stripe-not-loaded" };
  const scripts = Array.from(document.scripts).filter(script => {
    try {
      const url = new URL(script.src);
      return url.origin === "https://js.stripe.com" && (url.pathname === "/v3" || url.pathname === "/v3/");
    } catch { return false; }
  });
  if (scripts.length !== 1) return { status: "ambiguous-script" };
  document.__nanocodexSageStripeLoadRecovered = true;
  scripts[0].dispatchEvent(new Event("load"));
  return { status: "dispatched" };
})()`;

export const KITESURF_SAGE_RECOVERY_GUIDANCE = [
  "Known Kitesurf compatibility recovery: Sage Pilates Arketa calendar/schedule checkout can stall at only Login although Stripe has executed. This is a narrowly scoped missing script-load notification workaround, not an engine fix. Keep the native upstream CDP API unchanged.",
  "Only when that authorized public checkout is still exactly Login after at least 6 seconds, use native cdp.send({method: 'Runtime.evaluate', sessionId, params: {expression: <the expression below>, returnByValue: true}}) in the same browser_execute call. The expression verifies the exact site/path and Stripe script, requires window.Stripe to be a function, and permits one dispatch per document. Do not broaden the checks, replace Stripe, or replay arbitrary script events.",
  SAGE_STRIPE_LOAD_RECOVERY_EXPRESSION,
  "After a dispatched result, wait up to 7 seconds and inspect visible checkout class/package pricing and login content. Dispatch alone is not success. Report a remaining stall; do not dispatch again or log in, reserve, or pay without the user's authorization.",
].join("\n");
