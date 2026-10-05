import { createServicesClient, createHostedRequest, openHostedPopup, ServiceError, serviceResource, type ServicesClient } from 'nanocodex/services';
import { Client } from 'nanocodex/connect';
const direct: ServicesClient = createServicesClient({ apiKey: 'server-only-example' });
const connect = Client.create({ appId: 'services-example', session: false });
const connection = await connect.connection.connect({ capabilities: { services: { vault: { ids: ['opaque-vault-item'], origins: ['https://example.com'], request: true }, phone: { numberIds: [], read: true, provision: true, release: false } } } });
const scoped = createServicesClient({ connect, grantId: connection.grant.id });
const entries = await scoped.vault.list();
for (const entry of entries.vault) {
  if (entry.kind === 'totp') {
    const issuer: string = entry.issuer;
    // @ts-expect-error Vault seeds never leave the broker
    entry.seed;
    void issuer;
  }
}
const receipt = await direct.vault.request({ vault_id: 'opaque-vault-item', url: 'https://example.com', method: 'POST', body_encoding: 'json', body: '{"code":"{{NANOCODEX_VAULT_TOTP}}"}' });
const status: number = receipt.status;
// @ts-expect-error Broker responses are status-only
receipt.body;
await scoped.phone.provision({ operation_id: crypto.randomUUID(), phone_number: '+12025550101', country: 'US' });
await scoped.phone.release('number-id', { operation_id: crypto.randomUUID() });
// @ts-expect-error Writes require a caller-retained operation ID
await scoped.phone.provision({ phone_number: '+12025550101', country: 'US' });
// @ts-expect-error Approval remains in the hosted account UI
scoped.phone.approve({});
// @ts-expect-error Direct and Connect credentials are exclusive
createServicesClient({ connect, grantId: 'grant', apiKey: 'key' });
const hosted = createHostedRequest({ appOrigin: 'https://example.com' });
const metadata = await openHostedPopup(hosted);
// @ts-expect-error Enrollment returns metadata only
metadata.code;
serviceResource({ phone: { numberIds: [], read: true, provision: false, release: false } });
const error = new ServiceError('unknown', { outcomeUnknown: true });
void [status, error];
const selection = createHostedRequest({ service: 'vault', action: 'select', appOrigin: 'https://example.com' });
const selected = await openHostedPopup(selection);
if (selected.service === 'vault' && selected.action === 'select') {
  const id: string = selected.vault_id;
  const kind: import('nanocodex/services').VaultMetadata['kind'] = selected.kind;
  // @ts-expect-error Selection shares identity only, not private metadata
  selected.username;
  void [id, kind];
}
// @ts-expect-error Selection applies only to Vault
createHostedRequest({ service: 'phone', operationId: crypto.randomUUID(), action: 'select', appOrigin: 'https://example.com' });
