export interface Env {
  DEVICES: KVNamespace;
  OPENROUTER_API_KEY: string;
  OPENROUTER_BASE: string;
  FREE_MODEL: string;
  FREE_REQUEST_LIMIT: string;
}

interface DeviceRecord {
  device_id: string;
  free_requests_used: number;
  coin_balance_microdollars: number; // 1 coin = 200_000 µ$; avoids float rounding
  tier: 'free' | 'paid';
  registered_at: string;
}

// ── Router ────────────────────────────────────────────────────────────────────

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    // CORS preflight — needed when testing from a browser
    if (request.method === 'OPTIONS') {
      return corsOk();
    }

    const url = new URL(request.url);
    const { pathname } = url;

    if (pathname === '/health' && request.method === 'GET') {
      return json({ ok: true, version: '0.5.0' });
    }
    if (pathname === '/register' && request.method === 'POST') {
      return withCors(await handleRegister(request, env));
    }
    if (pathname === '/ai/relay' && request.method === 'POST') {
      return withCors(await handleRelay(request, env));
    }
    if (pathname === '/balance' && request.method === 'GET') {
      return withCors(await handleBalance(request, env));
    }

    return json({ error: 'not_found' }, 404);
  },
};

// ── Handlers ──────────────────────────────────────────────────────────────────

async function handleRegister(request: Request, env: Env): Promise<Response> {
  let body: { device_id?: unknown };
  try {
    body = await request.json<{ device_id?: unknown }>();
  } catch {
    return json({ error: 'invalid_json' }, 400);
  }

  const device_id = body?.device_id;
  if (typeof device_id !== 'string' || device_id.length < 8) {
    return json({ error: 'device_id required' }, 400);
  }

  // Idempotent: re-registering the same device_id returns the existing token.
  const existing = await env.DEVICES.get(`by_device:${device_id}`);
  if (existing) {
    const rec = JSON.parse(existing) as { proxy_token: string; device: DeviceRecord };
    return json({
      proxy_token: rec.proxy_token,
      free_remaining: freeRemaining(rec.device, env),
    });
  }

  const proxy_token = crypto.randomUUID();
  const device: DeviceRecord = {
    device_id,
    free_requests_used: 0,
    coin_balance_microdollars: 0,
    tier: 'free',
    registered_at: new Date().toISOString(),
  };

  await Promise.all([
    env.DEVICES.put(`token:${proxy_token}`, JSON.stringify(device)),
    env.DEVICES.put(`by_device:${device_id}`, JSON.stringify({ proxy_token, device })),
  ]);

  return json({ proxy_token, free_remaining: freeLimit(env) });
}

async function handleRelay(request: Request, env: Env): Promise<Response> {
  const auth = await authenticate(request, env);
  if ('error' in auth) return json({ error: auth.error }, 401);
  const { record, proxy_token } = auth;

  // Enforce free-tier limit
  if (record.tier === 'free' && record.free_requests_used >= freeLimit(env)) {
    return json(
      {
        error: 'free_trial_exhausted',
        message:
          'Your 50 free requests have been used. Purchase coins in the app to continue.',
        free_remaining: 0,
      },
      402,
    );
  }

  // Parse and validate the upstream payload
  let payload: Record<string, unknown>;
  try {
    payload = await request.json<Record<string, unknown>>();
  } catch {
    return json({ error: 'invalid_json' }, 400);
  }

  // Free tier always uses the free vision model regardless of what the app requested
  if (record.tier === 'free') {
    payload.model = env.FREE_MODEL;
  }

  // Forward to OpenRouter
  const upstream = await fetch(`${env.OPENROUTER_BASE}/chat/completions`, {
    method: 'POST',
    headers: {
      Authorization: `Bearer ${env.OPENROUTER_API_KEY}`,
      'Content-Type': 'application/json',
      'HTTP-Referer': 'https://ai-navigator.app',
      'X-Title': 'AI Navigator',
    },
    body: JSON.stringify(payload),
  });

  // Increment usage counter — fire-and-forget so the response isn't delayed.
  // KV write is eventually consistent; a brief race (double-relay) is acceptable.
  if (record.tier === 'free') {
    const updated: DeviceRecord = {
      ...record,
      free_requests_used: record.free_requests_used + 1,
    };
    void env.DEVICES.put(`token:${proxy_token}`, JSON.stringify(updated));
    // Keep the by_device index in sync
    void env.DEVICES.get(`by_device:${record.device_id}`).then((raw) => {
      if (!raw) return;
      const idx = JSON.parse(raw) as { proxy_token: string; device: DeviceRecord };
      idx.device = updated;
      void env.DEVICES.put(`by_device:${record.device_id}`, JSON.stringify(idx));
    });
  }

  const remaining = Math.max(0, freeLimit(env) - record.free_requests_used - 1);

  // Stream the upstream response back verbatim
  return new Response(upstream.body, {
    status: upstream.status,
    headers: {
      'Content-Type': upstream.headers.get('Content-Type') ?? 'application/json',
      'X-Free-Remaining': String(remaining),
      'Access-Control-Allow-Origin': '*',
    },
  });
}

async function handleBalance(request: Request, env: Env): Promise<Response> {
  const auth = await authenticate(request, env);
  if ('error' in auth) return json({ error: auth.error }, 401);
  const { record } = auth;

  return json({
    tier: record.tier,
    free_remaining: freeRemaining(record, env),
    coin_balance_microdollars: record.coin_balance_microdollars,
  });
}

// ── Auth ──────────────────────────────────────────────────────────────────────

type AuthOk = { record: DeviceRecord; proxy_token: string };
type AuthErr = { error: string };

async function authenticate(
  request: Request,
  env: Env,
): Promise<AuthOk | AuthErr> {
  const header = request.headers.get('Authorization') ?? '';
  if (!header.startsWith('Bearer ')) {
    return { error: 'missing_authorization' };
  }
  const proxy_token = header.slice(7).trim();
  if (!proxy_token) return { error: 'missing_authorization' };

  const raw = await env.DEVICES.get(`token:${proxy_token}`);
  if (!raw) return { error: 'invalid_token' };

  return { record: JSON.parse(raw) as DeviceRecord, proxy_token };
}

// ── Helpers ───────────────────────────────────────────────────────────────────

function freeLimit(env: Env): number {
  return parseInt(env.FREE_REQUEST_LIMIT ?? '50', 10);
}

function freeRemaining(record: DeviceRecord, env: Env): number {
  return Math.max(0, freeLimit(env) - record.free_requests_used);
}

function json(data: unknown, status = 200): Response {
  return new Response(JSON.stringify(data), {
    status,
    headers: {
      'Content-Type': 'application/json',
      'Access-Control-Allow-Origin': '*',
    },
  });
}

function corsOk(): Response {
  return new Response(null, {
    status: 204,
    headers: {
      'Access-Control-Allow-Origin': '*',
      'Access-Control-Allow-Methods': 'GET, POST, OPTIONS',
      'Access-Control-Allow-Headers': 'Content-Type, Authorization',
    },
  });
}

function withCors(response: Response): Response {
  response.headers.set('Access-Control-Allow-Origin', '*');
  return response;
}
