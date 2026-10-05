# Google account sign-in

Google login uses a dedicated Google OAuth web client. Configure these secrets on the managed Worker:

- `GOOGLE_SIGN_IN_CLIENT_ID`
- `GOOGLE_SIGN_IN_CLIENT_SECRET`

Register each deployed account origin's exact HTTPS redirect URI in that client, for example `https://account.example/v1/auth/google/callback`. Register development callback origins separately when testing Google interactively. The server derives this URI from the request origin; clients cannot supply a redirect target. Missing configuration returns `google_sign_in_unavailable` (503).

This flow requests `openid email`. It does not connect Google Workspace, grant Gmail/Drive/Calendar access, or reuse connector credentials. Google Workspace connector consent remains a separate action. No provider tokens are retained. Identity is the verified Google issuer and subject, never an email address.

## HTTP contract

All POSTs require JSON and an Origin header equal to the account API origin; native clients explicitly set this header. Responses are not cacheable. Attempts expire after five minutes.

- `POST /v1/auth/google/start`: `{mode:"browser"|"native",code_challenge,intent?:"sign_in"|"link"}`. The challenge is base64url SHA-256 of a fresh client-held RFC7636 verifier. Returns `{attempt_id,authorization_url,expires_in:300}`. Browser callers retain the returned HttpOnly cookie and open `authorization_url` in a popup; native callers open it using ASWebAuthenticationSession.
- `GET /v1/auth/google/authorize?attempt_id=...`: binds the provider round-trip to its browser with a short-lived HttpOnly cookie, then redirects to Google. Browser mode requires the start cookie. Native mode initializes the separate authentication browser's cookie. The server generates independent state, nonce and Google PKCE values.
- `POST /v1/auth/google/status`: `{attempt_id,code_verifier}` returns `{status:"pending"|"ready"|"failed"|"cancelled",error?}`. It never returns an identity, provider token or completion code.
- `POST /v1/auth/google/complete`: `{attempt_id,code_verifier,completion_code?}` consumes a ready attempt once and returns `{user:{id,address,persistent:true}}` with the standard HttpOnly `nanocodex_account=s_...` cookie. Native mode additionally requires `completion_code` from the native callback. Native clients then use the existing authenticated `/v1/api-keys` endpoint.
- `POST /v1/auth/google/cancel`: proof as above, returns 204 on cancellation. A cancelled attempt's status remains readable until expiry. Repeating cancel returns 204; completion of a cancelled attempt returns `invalid_or_expired_google_attempt` (400).
- `GET /v1/auth/google/callback`: registered Google redirect URI. Browser mode displays a closing page; the initiating page polls and completes without losing its Connect dialog. Native success redirects only to `nanocodex://auth/google?attempt_id=...&status=ready&completion_code=...`; failure has `attempt_id` and `status=failed`. The completion code is a random single-use exchange receipt, protected by client PKCE. No account session, API key or Google token is placed in this URL. Native clients must validate the callback scheme/host/path, exact query fields, and attempt ID before exchange.

Requiring native callback possession prevents someone from forwarding an authorization URL to another person and using their own pre-held verifier to claim that person's Google login by polling.

Normal sign-in resolves an existing Google subject or creates a new account. It does not promote an anonymous account: independent SMS and Google attempts could otherwise race to attach identities. To add Google to an existing phone account, a signed-in browser explicitly starts `intent:"link"`. The same persistent account session must be present at completion. A Google identity already owned by another account returns `google_identity_already_linked` (409); accounts are never merged.

Proof failures return `invalid_google_attempt` (400); native callback-proof failure returns `invalid_google_completion` (400). Expired/consumed attempts return `invalid_or_expired_google_attempt` (400). Pending completion returns `google_authorization_pending` (409). Provider denial returns `google_access_denied`; invalid tokens or provider failures return `google_authorization_failed`. Link requests without a persistent browser account return `google_link_requires_account` (401); a changed account at completion returns `google_link_requires_same_account` (403). After an uncertain complete result, restart sign-in rather than retrying a consumed exchange.

Run `pnpm --dir js/managed test:google-sign-in` for a public HTTP journey against the real Worker auth router and durable objects. Only Google token/JWKS and external wallet provisioning are synthetic. The journey signs real RSA JWTs and records status evidence in ignored `output/google-sign-in/http-trace.json`.
