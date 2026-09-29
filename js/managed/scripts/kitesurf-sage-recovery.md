# Sage checkout recovery in native Kitesurf

Sage's Arketa checkout can remain at `Login` after its async Stripe script has
executed. The parser-inserted script's `load` listeners are not notified in the
observed Kitesurf engine. Replaying that notification once allows the site's
existing Stripe loader to continue. This is an agent-executed compatibility
recipe, not an engine fix or an automatic browser wrapper.

The Kitesurf agent instructions include the exact recipe from
`src/kitesurf-sage-recovery.ts`. It uses upstream `Runtime.evaluate`, checks the
exact Sage checkout origin/path, an unchanged `Login` body after six seconds,
a loaded Stripe global and one matching script. A document marker prevents a
second dispatch. Other sites and already rendered checkouts are untouched.
Success requires visible checkout details after the event, not merely a
successful dispatch. Authentication, reservation and payment are outside this
recovery's scope.

## Live branch test

From `js/managed`, with dependencies installed and Wrangler authorized for
Browser Run:

```sh
corepack pnpm exec wrangler dev --config scripts/wrangler.kitesurf-sage-recovery.jsonc --ip 127.0.0.1 --port 8802
```

Supply a current public Sage checkout URL in another terminal:

```sh
curl --fail-with-body --max-time 100 http://127.0.0.1:8802/smoke \
  -H 'Content-Type: application/json' \
  --data '{"checkoutUrl":"https://app.arketa.co/iframe/sagepilates/calendar/checkout/CLASS_ID"}'
```

The fixture calls this branch's `createManagedBrowserRuntime` over HTTP with
real remote `env.BROWSER`, local Worker Loader and the upstream browser tool.
It first checks scope rejection, records the stalled baseline, executes the
same recipe supplied to the agent, checks rendered class/pricing content, and
checks duplicate prevention. It returns bounded public evidence and closes the
browser. It does not test an LLM deciding to use the recipe or authenticated
booking/payment. An upstream fix or changed website can make the baseline
assertion fail; investigate rather than treating that as a reason to dispatch.

Stop Wrangler after testing. This fixture is for local development; do not deploy.
