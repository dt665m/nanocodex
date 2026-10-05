import { completeVaultEnrollment, completeVaultSelection, enrollmentTarget } from "./serviceEnrollment";
import "./DeviceConnect.css";
import "./Vault.css";
import { useAccountQuery } from "./useAccountQuery";
import { KeyRound, LockKeyhole, Plus, Trash2, X } from "lucide-react";
import {
  useCallback,
  useEffect,
  useId,
  useRef,
  useState,
  type FormEvent,
  type InputHTMLAttributes,
  type ReactNode,
  type RefObject,
} from "react";

import { AccountChooser } from "nanocodex-connect-ui/AccountChooser";
import { responseFailure, useAccountSession } from "./AccountSession";
import { clientFailureMessage } from "./clientFailure";
import { useModalBoundary } from "./modalBoundary";
import { SshIdentityManager } from "./SshIdentityManager";
import { decodeSshIdentities, type SshIdentityMetadata } from "./sshIdentities";
import {
  decodeVaultEntries,
  vaultEntryPath,
  type VaultEntryKind,
  type VaultEntryMetadata,
} from "./vaultEntries";

type VaultStatus = Readonly<{
  ssh: readonly SshIdentityMetadata[];
  entries: readonly VaultEntryMetadata[];
}>;

const sections: readonly Readonly<{
  kind: VaultEntryKind;
  title: string;
  addLabel: string;
}>[] = [
  { kind: "totp", title: "Authenticator accounts", addLabel: "Add authenticator" },
  { kind: "login", title: "Logins", addLabel: "Add login" },
  { kind: "api_key", title: "API keys", addLabel: "Add API key" },
  { kind: "card", title: "Cards", addLabel: "Add card" },
  { kind: "address", title: "Addresses", addLabel: "Add address" },
  { kind: "phone", title: "Phones", addLabel: "Add phone" },
];

export function Vault() {
  const session = useAccountSession();
  const service = new URLSearchParams(window.location.search).get("service");
  const totpOnly = service === "totp";
  const selecting = service === "select";
  const recipient = enrollmentTarget();
  const [selectedId, setSelectedId] = useState("");
  const refreshSession = session.refresh;
  const accountId = session.account?.persistent ? session.account.id : undefined;
  const [uncertain, setUncertain] = useState(false);
  const [savedNotice, setSavedNotice] = useState<string>();
  const [operationFailure, setFailure] = useState<string | null>(null);
  const [operation, setOperation] = useState<string | null>(null);
  const [adding, setAdding] = useState<VaultEntryKind | null>(null);
  const dialogReturnFocusRef = useRef<HTMLElement | null>(null);
  const closeDialog = useCallback(() => setAdding(null), []);

  const { query, refresh } = useAccountQuery(accountId, "/v1/credentials", decodeVaultStatus);
  const status = query.data ?? null;
  const failure = operationFailure ?? (query.error ? clientFailureMessage(query.error, "Couldn’t load your vault.") : null);
  const load = useCallback(async () => {
    setFailure(null);
    await refresh();
  }, [refresh]);

  useEffect(() => {
    setFailure(null);
    setAdding(null);
    setSelectedId("");
  }, [accountId]);

  const save = async (kind: VaultEntryKind, values: Record<string, string>) => {
    if (operation || uncertain) return;
    setOperation(`add:${kind}`);
    setFailure(null);
    try {
      const response = await vaultRequest(vaultEntryPath(kind), {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(values),
      });
      if (response.status === 401) {
        await response.body?.cancel();
        await refreshSession();
        return;
      }
      if (!response.ok) {
        if (kind === "totp") {
          await response.body?.cancel();
          if ([400, 403, 422].includes(response.status)) { setFailure("Couldn’t save the authenticator. Check the fields and your access."); return; }
          throw new Error("unconfirmed_save");
        }
        throw await responseFailure(response, `Couldn’t add the ${kind}.`);
      }
      if (kind === "totp") {
        const wire: unknown = await response.json();
        const entry = decodeVaultEntries([wire])[0]!;
        if (!isRecord(wire) || entry.kind !== "totp" || wire.origin !== values.origin) throw new Error("Invalid authenticator receipt");
        if (totpOnly) completeVaultEnrollment(entry, values.origin!);
        setSavedNotice("Authenticator saved to Vault.");
      } else { await response.body?.cancel(); }
      await load();
      setAdding(null);
    } catch (cause) {
      if (kind === "totp") {
        setUncertain(true); setAdding(null);
        setFailure("The save could not be confirmed. Check your Vault before adding this authenticator again.");
      } else setFailure(clientFailureMessage(cause, `Couldn’t add the ${kind}. Check every field and try again.`));
    } finally {
      setOperation(null);
    }
  };

  const remove = async (entry: VaultEntryMetadata) => {
    if (operation) return;
    setOperation(entry.id);
    setFailure(null);
    try {
      const response = await vaultRequest(vaultEntryPath(entry.kind, entry.id), { method: "DELETE" });
      if (response.status === 401) {
        await response.body?.cancel();
        await refreshSession();
        return;
      }
      if (!response.ok) throw await responseFailure(response, "Couldn’t delete the vault item.");
      await response.body?.cancel();
      await load();
    } catch (cause) {
      setFailure(clientFailureMessage(cause, "Couldn’t delete the vault item."));
    } finally {
      setOperation(null);
    }
  };

  if (window.top !== window) return <div className="vault-page"><h1>Open Vault in a secure window</h1><p>Authenticator enrollment and credential forms are available only in a top-level window. Use the app’s enrollment button to open a secure popup.</p></div>;
  if (session.status === "checking") return null;
  if (!accountId) {
    return (
      <div className="wizard-page wizard-account-page vault-sign-in">
        <header className="wizard-intro">
          <div className="wizard-app">
            <h1>Vault</h1>
            <p>Verify your phone before storing encrypted credentials and personal details.</p>
          </div>
        </header>
        <AccountChooser
          description={session.reauthenticationRequired
            ? "Your session expired. Enter your phone number to restore your vault."
            : "Enter your phone number to create or restore your Nanocodex account."}
          disabled={session.operation !== null}
          failure={session.error}
          onChooseAccount={(selection) => void session.chooseAccount(selection)}
        />
      </div>
    );
  }

  if (selecting) return <div className="vault-page"><div className="vault-content vault-picker">
    <header className="vault-heading"><div><h1>Choose a Vault item</h1></div></header>
    {recipient ? <>
      <p>Share the selected item’s name, kind, and Vault ID with <strong>{recipient.origin}</strong>.</p>
      <p>This selection does not grant access to use the item. You review that access separately in Connect.</p>
      {failure ? <p role="alert">{failure}</p> : null}
      {!status ? <p role="status">Loading your Vault…</p> : status.entries.length ? <>
        <ul className="vault-picker-list">{status.entries.map(entry => <li key={entry.id}>
          <label><input type="radio" name="vault-selection" value={entry.id} checked={selectedId === entry.id} onChange={() => {setSelectedId(entry.id); setSavedNotice(undefined);}} /><span><strong>{entry.name}</strong><small>{labelForKind(entry.kind)}</small></span></label>
        </li>)}</ul>
        <button className="vault-save" type="button" disabled={!status.entries.some(entry => entry.id === selectedId)} onClick={() => {
          const entry = status.entries.find(item => item.id === selectedId);
          if (entry && completeVaultSelection(entry)) setSavedNotice(`Shared ${entry.name} with ${recipient.origin}.`);
          else setFailure("The requesting window is unavailable. Reopen the picker from your app.");
        }}>Share selected item with {recipient.origin}</button>
      </> : <p>No saved Vault items. Add an item in your Vault, then reopen this picker.</p>}
      {savedNotice ? <p role="status">{savedNotice}</p> : null}
    </> : <p role="alert">This picker needs a valid requesting origin and state. Reopen it from your app.</p>}
  </div></div>;

  return (
    <div className="vault-page">
      <div className="vault-content">
        <header className="vault-heading">
          <div>
            <span>Private broker</span>
            <h1>Vault</h1>
          </div>
          <p>Credentials and authenticator setup keys stay encrypted; only account metadata remains available after saving.</p>
        </header>

        {savedNotice ? <p role="status">{savedNotice}</p> : null}
        {totpOnly && enrollmentTarget() ? <p>After saving, share the authenticator’s name and website with {enrollmentTarget()!.origin}. The setup key stays private.</p> : null}
        {session.error || failure ? (
          <div className="account-failure vault-failure" role="alert">
            <p>{session.error ?? failure}</p>
            <button type="button" onClick={() => void load()}>Retry</button>
          </div>
        ) : null}

        {!totpOnly ? <div className="vault-ssh">
          <SshIdentityManager
            key={accountId}
            disabled={operation !== null}
            identities={status?.ssh ?? null}
            onChanged={load}
            presentation="wizard"
            refreshSession={refreshSession}
            title="SSH keys"
          />
        </div> : null}

        {sections.filter(section => !totpOnly || section.kind === "totp").map((section) => {
          const entries = status?.entries.filter((entry) => entry.kind === section.kind) ?? [];
          return (
            <section className="vault-section" aria-labelledby={`vault-${section.kind}-title`} key={section.kind}>
              <div className="vault-section-heading">
                <div>
                  <span>Personal data</span>
                  <h2 id={`vault-${section.kind}-title`}>{section.title}</h2>
                </div>
                <small>{status ? `${entries.length} saved` : "Loading"}</small>
              </div>
              {entries.length ? (
                <ul className="vault-entry-list">
                  {entries.map((entry) => (
                    <li key={entry.id}>
                      <div className="vault-entry-icon" aria-hidden="true"><LockKeyhole /></div>
                      <div>
                        <strong>{entry.name}</strong>
                        <span>{labelForKind(entry.kind)} · encrypted</span>
                      </div>
                      <button
                        aria-label={`Delete ${entry.name}`}
                        disabled={operation !== null}
                        onClick={() => void remove(entry)}
                        type="button"
                      ><Trash2 aria-hidden="true" /></button>
                    </li>
                  ))}
                </ul>
              ) : null}
              <button
                className="vault-add"
                disabled={!status || operation !== null || (uncertain && section.kind === "totp")}
                onClick={(event) => {
                  dialogReturnFocusRef.current = event.currentTarget;
                  setAdding(section.kind);
                }}
                type="button"
              >
                <Plus aria-hidden="true" />
                {section.addLabel}
              </button>
            </section>
          );
        })}
      </div>
      {adding ? (
        <VaultEntryDialog
          busy={operation !== null}
          kind={adding}
          error={failure}
          completionOrigin={totpOnly ? enrollmentTarget()?.origin : undefined}
          onClose={closeDialog}
          onSave={save}
          returnFocusRef={dialogReturnFocusRef}
        />
      ) : null}
    </div>
  );
}

export function VaultEntryDialog({
  busy,
  kind,
  onClose,
  onSave,
  returnFocusRef,
  name = "",
  origin,
  title,
  description = "Values are encrypted in your vault.",
  error,
  children,
  completionOrigin,
}: Readonly<{
  busy: boolean;
  kind: VaultEntryKind;
  onClose(): void;
  onSave(kind: VaultEntryKind, values: Record<string, string>): Promise<void>;
  returnFocusRef: RefObject<HTMLElement | null>;
  name?: string;
  origin?: string;
  title?: string;
  description?: string;
  error?: string | null;
  children?: ReactNode;
  completionOrigin?: string;
}>) {
  const titleId = useId();
  const [shareCompletion, setShareCompletion] = useState(false);
  const [validationError, setValidationError] = useState<string>();
  const backdropRef = useRef<HTMLDivElement>(null);
  const dialogRef = useRef<HTMLElement>(null);
  const firstInputRef = useRef<HTMLInputElement>(null);
  const dismiss = useCallback(() => {
    if (!busy) onClose();
  }, [busy, onClose]);
  useModalBoundary({
    backdropRef,
    initialFocusRef: firstInputRef,
    onDismiss: dismiss,
    open: true,
    panelRef: dialogRef,
    returnFocusRef,
  });

  const submit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (completionOrigin && !shareCompletion) return;
    const data = new FormData(event.currentTarget);
    const values = Object.fromEntries(
      [...data.entries()].flatMap(([key, value]) => typeof value === "string" && value.trim()
        ? [[key, key === "password" || key === "api_key" ? value : value.trim()]]
        : []),
    );
    if (kind === "totp") {
      try {
        const url = new URL(values.origin ?? "");
        if (url.protocol !== "https:" || url.origin !== values.origin || url.username || url.password) throw new Error();
      } catch { setValidationError("Enter an exact HTTPS origin, such as https://example.com, without a path or trailing slash."); return; }
    }
    setValidationError(undefined);
    // Clear private inputs before handing off the ephemeral same-origin request.
    for (const input of event.currentTarget.querySelectorAll<HTMLInputElement>('input[type="password"]')) input.value = "";
    void onSave(kind, values).finally(() => {
      for (const key of Object.keys(values)) delete values[key];
    });
  };

  return (
    <div className="vault-dialog-backdrop" ref={backdropRef} onMouseDown={(event) => {
      if (event.target === event.currentTarget && !busy) onClose();
    }}>
      <section aria-labelledby={titleId} aria-modal="true" className="vault-dialog" ref={dialogRef} role="dialog">
        <header>
          <div>
            <h2 id={titleId}>{title ?? `Add ${kind === "api_key" ? "API key" : labelForKind(kind).toLowerCase()}`}</h2>
            <p>{description}</p>
          </div>
          <button aria-label="Close" disabled={busy} onClick={onClose} type="button"><X aria-hidden="true" /></button>
        </header>
        {children}
        {error || validationError ? <p role="alert">{error ?? validationError}</p> : null}
        <form autoComplete="off" onSubmit={submit}>
          <div className="vault-dialog-fields">
            <VaultField autoComplete="off" defaultValue={name} inputRef={firstInputRef} label="Name" maxLength={120} name="name" placeholder={namePlaceholder(kind)} required />
            {kind === "login" ? <VaultField autoComplete="off" defaultValue={origin} label="Website (optional)" maxLength={2048} name="browser_origin" placeholder="https://example.com" type="url" /> : null}
            {kind === "totp" ? <VaultField autoComplete="off" defaultValue={origin} label="Website origin" maxLength={2048} name="origin" placeholder="https://example.com" type="url" required /> : null}
            {fieldsForKind(kind)}
          </div>
          {completionOrigin ? <label className="vault-share-consent"><input type="checkbox" checked={shareCompletion} onChange={event => setShareCompletion(event.target.checked)} />Share completion with {completionOrigin}. This includes the saved name, website, and Vault ID.</label> : null}
          <footer>
            <button disabled={busy} onClick={onClose} type="button">Cancel</button>
            <button className="vault-save" disabled={busy || Boolean(completionOrigin && !shareCompletion)} type="submit">{busy ? "Saving…" : "Save"}</button>
          </footer>
        </form>
      </section>
    </div>
  );
}

function fieldsForKind(kind: VaultEntryKind): ReactNode {
  if (kind === "totp") return <TotpFields />;
  if (kind === "api_key") return <VaultField autoCapitalize="none" autoComplete="off" label="API key" maxLength={8192} name="api_key" required spellCheck={false} type="password" secure />;
  // This edits a third-party credential, rather than signing in to this site.
  // Explicitly disable autocomplete on both fields: Chromium's address-on-typing
  // suggestions can otherwise mix with login suggestions and crash Brave 1.95.
  if (kind === "login") return <>
    <VaultField autoCapitalize="none" autoComplete="off" label="Username" maxLength={512} name="username" required spellCheck={false} />
    <VaultField autoComplete="off" label="Password" maxLength={8192} name="password" required type="password" secure />
  </>;
  if (kind === "card") return <>
    <VaultField autoComplete="cc-number" inputMode="numeric" label="Card number" maxLength={23} name="card_number" required secure />
    <div className="vault-field-row">
      <VaultField autoComplete="cc-exp-month" inputMode="numeric" label="Expiry month" maxLength={2} name="expiry_month" pattern="(?:0?[1-9]|1[0-2])" required />
      <VaultField autoComplete="cc-exp-year" inputMode="numeric" label="Expiry year" maxLength={4} minLength={4} name="expiry_year" pattern="[0-9]{4}" required />
    </div>
    <div className="vault-field-row">
      <VaultField autoComplete="cc-csc" inputMode="numeric" label="CVV" maxLength={4} minLength={3} name="cvv" pattern="[0-9]{3,4}" required secure type="password" />
      <VaultField autoComplete="postal-code" label="Billing ZIP" maxLength={32} name="billing_zip" required />
    </div>
  </>;
  if (kind === "address") return <>
    <VaultField autoComplete="address-line1" label="Address line 1" maxLength={256} name="address_line_1" required />
    <div className="vault-field-row">
      <VaultField autoComplete="address-level2" label="City" maxLength={120} name="city" required />
      <VaultField autoComplete="address-level1" label="State" maxLength={120} name="state" required />
    </div>
    <div className="vault-field-row">
      <VaultField autoComplete="postal-code" label="ZIP" maxLength={32} name="zip" required />
      <VaultField autoComplete="country-name" label="Country" maxLength={120} name="country" required />
    </div>
    <details className="vault-advanced">
      <summary>Advanced · Address line 2</summary>
      <VaultField autoComplete="address-line2" label="Address line 2" maxLength={256} name="address_line_2" />
    </details>
  </>;
  return <VaultField autoComplete="tel" inputMode="tel" label="Phone number" maxLength={64} name="phone_number" required />;
}

function TotpFields() {
  const [mode, setMode] = useState("uri");
  return <>
    <label className="vault-field"><span>Enrollment format</span><select value={mode} onChange={event => setMode(event.target.value)}><option value="uri">Authenticator URI</option><option value="seed">Setup key</option></select></label>
    {mode === "uri" ? <VaultField key="uri" label="Authenticator URI" name="otpauth_uri" type="password" autoComplete="off" autoCapitalize="none" spellCheck={false} maxLength={4096} required secure /> : <>
      <VaultField key="seed" label="Setup key" name="seed" type="password" autoComplete="off" autoCapitalize="none" spellCheck={false} maxLength={208} required secure />
      <VaultField label="Issuer" name="issuer" maxLength={256} required />
      <VaultField label="Account label" name="account" maxLength={256} required />
    </>}
    <p>Paste your otpauth URI or setup key here. It goes directly to your encrypted Vault. Only account metadata is returned.</p>
  </>;
}

function VaultField({ inputRef, label, secure = false, ...input }: Readonly<{
  inputRef?: RefObject<HTMLInputElement | null>;
  label: string;
  secure?: boolean;
}> & InputHTMLAttributes<HTMLInputElement>) {
  const id = useId();
  return (
    <label className="vault-field" htmlFor={id}>
      <span>{secure ? <KeyRound aria-hidden="true" /> : null}{label}</span>
      <input id={id} ref={inputRef} {...input} />
    </label>
  );
}

function labelForKind(kind: VaultEntryKind): string {
  return kind === "totp" ? "Authenticator" : kind === "api_key" ? "API key" : kind === "login" ? "Login" : kind === "card" ? "Card" : kind === "address" ? "Address" : "Phone";
}

function namePlaceholder(kind: VaultEntryKind): string {
  if (kind === "card") return 'e.g. "Amex", "Chase"';
  if (kind === "address") return 'e.g. "Home", "Office"';
  if (kind === "phone") return 'e.g. "Mobile", "Work"';
  return 'e.g. "Gmail", "GitHub"';
}

async function vaultRequest(path: string, init: RequestInit = {}): Promise<Response> {
  return fetch(path, {
    ...init,
    cache: "no-store",
    redirect: "error",
    referrerPolicy: "no-referrer",
    credentials: "same-origin",
    headers: {
      accept: "application/json",
      ...Object.fromEntries(new Headers(init.headers)),
    },
  });
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}

function decodeVaultStatus(value: unknown): VaultStatus {
  if (!isRecord(value)) throw new Error("Invalid vault response.");
  return { ssh: decodeSshIdentities(value.ssh), entries: decodeVaultEntries(value.vault) };
}
