/** Standalone account services. This entry point has no agent or WASM dependencies. */
export { createHostedRequest, readHostedResult, openHostedPopup } from './hosted.mjs';
export { normalizeServices, serviceResource } from './scope.mjs';

export class ServiceError extends Error {
  constructor(message, { status, code, outcomeUnknown = false } = {}) {
    super(message);
    this.name = 'ServiceError';
    this.status = status;
    this.code = code;
    this.outcomeUnknown = outcomeUnknown;
  }
}

/** Every mutation is dispatched at most once. Retain phone operation IDs for reconciliation. */
export function createServicesClient(options) {
  if (!options || typeof options !== 'object') throw new TypeError('Service client options are required');
  const connected = options.connect !== undefined;
  if (connected === (options.apiKey !== undefined)) throw new TypeError('Choose an API key or a Connect client');
  const base = new URL(options.baseUrl ?? 'https://account.nanocodex.xyz');
  if (base.protocol !== 'https:' && !(base.protocol === 'http:' && ['localhost', '127.0.0.1', '[::1]'].includes(base.hostname))) {
    throw new TypeError('Service API requires HTTPS');
  }
  if (base.username || base.password || base.search || base.hash) throw new TypeError('Invalid service API URL');
  if (!connected && (typeof options.apiKey !== 'string' || !options.apiKey.trim())) throw new TypeError('API key is required');
  if (connected && typeof options.connect.fetch !== 'function') throw new TypeError('Connect client must expose fetch');
  const prefix = connected ? `/v1/grants/${segment(options.grantId)}/services` : '/v1/services';
  const fetcher = options.fetch ?? globalThis.fetch;
  async function request(path, method = 'GET', body, controls = {}) {
    const headers = { accept: 'application/json' };
    if (body !== undefined) headers['content-type'] = 'application/json';
    if (!connected) headers.authorization = `Bearer ${options.apiKey}`;
    if (connected && options.connect.appOrigin) headers.origin = options.connect.appOrigin;
    let response;
    let value;
    try {
      const init = { method, headers, redirect: 'manual', credentials: 'omit', signal: controls.signal,
        ...(body === undefined ? {} : { body: JSON.stringify(body) }) };
      response = connected
        ? await options.connect.fetch(prefix + path, init)
        : await fetcher(new URL(prefix + path, base), init);
      if ((response.status >= 300 && response.status < 400) || response.type === 'opaqueredirect') {
        throw new Error('Service API redirects are not followed');
      }
      value = response.status === 204 ? undefined : await response.json();
    } catch {
      throw new ServiceError(method === 'GET' ? 'Service request failed' : 'Service operation outcome is unknown; inspect its status before taking further action', {
        code: method === 'GET' ? 'transport_error' : 'outcome_unknown', outcomeUnknown: method !== 'GET',
      });
    }
    if (!response.ok) {
      const code = typeof value?.error === 'string' ? value.error : typeof value?.error?.code === 'string' ? value.error.code : undefined;
      throw new ServiceError(typeof code === 'string' ? code : `Service request failed (${response.status})`, {
        status: response.status, code, outcomeUnknown: method !== 'GET' && (response.status >= 500 || /outcome_unknown/.test(code ?? '')),
      });
    }
    return value;
  }
  return Object.freeze({
    catalog: (controls) => request('', 'GET', undefined, controls),
    vault: Object.freeze({
      list: async (controls) => ({ vault: (await request('/vault', 'GET', undefined, controls)).vault.map(vaultMetadata) }),
      get: async (id, controls) => ({ entry: vaultMetadata((await request(`/vault/${segment(id)}`, 'GET', undefined, controls)).entry) }),
      request: async (input, controls) => {
        const result = await request('/vault/request', 'POST', input, controls);
        if (!Number.isInteger(result?.status) || result.status < 100 || result.status > 599 || result.ok !== (result.status >= 200 && result.status < 300)) {
          throw new ServiceError('Invalid broker receipt; request outcome is unknown', { code: 'outcome_unknown', outcomeUnknown: true });
        }
        return Object.freeze({ status: result.status, ok: result.ok });
      },
    }),
    phone: Object.freeze({
      available: (query, controls) => request('/phone/numbers/available' + queryString(query), 'GET', undefined, controls),
      list: (controls) => request('/phone/numbers', 'GET', undefined, controls),
      provision: (input, controls) => request('/phone/numbers', 'POST', operation(input), controls),
      get: (id, controls) => request(`/phone/numbers/${segment(id)}`, 'GET', undefined, controls),
      release: (id, input, controls) => request(`/phone/numbers/${segment(id)}`, 'DELETE', operation(input), controls),
      messages: (id, query, controls) => request(`/phone/numbers/${segment(id)}/messages${queryString(query)}`, 'GET', undefined, controls),
      requests: Object.freeze({
        get: (id, controls) => request(`/phone/requests/${segment(id)}`, 'GET', undefined, controls),
      }),
    }),
  });
}

function segment(value) {
  if (typeof value !== 'string' || !/^[A-Za-z0-9_-]{1,128}$/.test(value)) throw new TypeError('An opaque service ID is required');
  return encodeURIComponent(value);
}
function operation(input) {
  if (!input || !/^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(input.operation_id)) {
    throw new TypeError('A stable UUID operation_id is required');
  }
  return input;
}
function queryString(query) {
  const params = new URLSearchParams();
  for (const [key, value] of Object.entries(query ?? {})) if (value !== undefined) params.set(key, String(value));
  return params.size ? '?' + params : '';
}
function vaultMetadata(entry) {
  const fields = { api_key: [], login: ['username', 'browser_origin'], card: ['last4'],
    address: ['address_line_1', 'address_line_2', 'city', 'state', 'zip', 'country'], phone: ['phone_number'],
    totp: ['issuer', 'account', 'origin', 'algorithm', 'digits', 'period'] };
  if (!entry || !Object.hasOwn(fields, entry.kind) || typeof entry.id !== 'string' || typeof entry.name !== 'string') {
    throw new ServiceError('Invalid Vault metadata', { code: 'invalid_response' });
  }
  return Object.freeze(Object.fromEntries(['id', 'kind', 'name', 'created_at', ...fields[entry.kind]]
    .filter(key => entry[key] !== undefined).map(key => [key, entry[key]])));
}
