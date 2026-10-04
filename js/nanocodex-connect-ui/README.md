# nanocodex-connect-ui

Reusable React account and Connect onboarding surfaces for Nanocodex.

```tsx
import {
  ConnectOnboarding,
  type ConnectOnboardingHost,
} from "nanocodex-connect-ui/App";
import "nanocodex-connect-ui/styles.css";

export function Approval({ host, request }) {
  return <ConnectOnboarding host={host} request={request} />;
}
```

The package owns browser presentation and ceremony orchestration. It does not
issue grants or enforce server-side Connect authority.

The Connect UI separates sign-in from authorization. Both steps fill the page without an overlay, rounded shell, or shadow. The sign-in form stays compact;
authorization pairs the requesting app and its origin with a review of accounts
and permissions. Desktop uses a two-column review; mobile stacks the same
information above persistent actions. The requester mark is an initial derived
from the validated app name, and Nanocodex uses its canonical mark.

The standalone host follows system appearance unless `data-theme="light"` or
`data-theme="dark"` is set. Styles remain scoped to `.connect-onboarding`.
The page owns its opaque background; the host frame fills the viewport. Reduced-motion preferences disable
control transitions. See the Connect dialog browser journeys for reproducible
light/dark mobile and desktop checks.
