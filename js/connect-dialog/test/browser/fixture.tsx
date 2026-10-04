import { useMemo, useState } from "react";
import { createRoot } from "react-dom/client";
import { ConnectOnboarding, type ConnectRequest } from "nanocodex-connect-ui/App";
import "nanocodex-connect-ui/styles.css";

const appOrigin = "http://atlas.nanocodex.localhost";
const request: ConnectRequest = {
  type: "walletConnect",
  id: "synthetic-modal-request",
  appId: "modal-journey",
  origin: appOrigin,
  rpc: {
    method: "wallet_connect",
    params: [{ capabilities: { auth: {
      url: `${window.location.origin}/v1/connect/auth`,
      resources: [
        "urn:nanocodex:app:modal-journey",
        `urn:nanocodex:origin:${encodeURIComponent(appOrigin)}`,
        "urn:nanocodex:authorization:hosted",
        "urn:nanocodex:agent:run",
        ...(new URLSearchParams(window.location.search).has("connections") ? [
          "urn:nanocodex:connectors:github,gmail,gcalendar",
          "urn:nanocodex:agent:output:final",
          "urn:nanocodex:agent:output:actions",
        ] : []),
      ],
    } } }],
  },
};

function Fixture() {
  const [outcome, setOutcome] = useState<string>();
  const host = useMemo(() => ({
    async respond(result: unknown) {
      (window as any).__hostReceipt = { kind: "approved", result };
      setOutcome("Request approved");
    },
    async reject(error?: unknown) {
      (window as any).__hostReceipt = { kind: "cancelled", error: String(error) };
      setOutcome("Request cancelled");
    },
  }), []);
  return outcome ? <p role="status">{outcome}</p> : <ConnectOnboarding host={host} request={request} />;
}

createRoot(document.getElementById("root")!).render(<Fixture />);
