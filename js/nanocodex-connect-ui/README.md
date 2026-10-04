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

The Connect sheet follows the native Swift account surfaces: adaptive neutral
colors from `ChatPalette`, system typography, inset grouped connections, and
persistent approval actions. It follows the system appearance unless the
standalone host sets `data-theme="light"` or `data-theme="dark"`. Styles stay
scoped to `.connect-onboarding`; the containing application owns the backdrop.
Long permission descriptions wrap, the sheet content scrolls independently of
its actions, and reduced-motion preferences disable control transitions.
