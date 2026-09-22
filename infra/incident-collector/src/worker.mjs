/**
 * XrayTun 现场包接收端点（Cloudflare Worker + R2）—— task-114
 *
 * 设计口径（与 PRIVACY.md 一一对应）：
 *   · 上传 **公开**（用户只需要点一下，不能要求他先注册）：所以必须有
 *     **大小上限 + 类型白名单（含 zip magic）+ 按 IP 的限流**；
 *   · **读取原始 zip / 删除** 需要 `X-Auth-Token`（只给维护者）；
 *   · manifest **公开安全**（不含日志正文、不含任何用户标识、不含 IP）；
 *   · **不把客户端 IP 写进 R2**：限流用「加盐哈希后的 IP」做键，且在内存里，不进持久层；
 *   · 保留 **30 天**：R2 lifecycle 兜底 + 本代码的**惰性过期**（读到超期就删并 404）。
 *
 * 无任何第三方依赖：只用 Web 标准 API + R2 绑定，测试用 node --test（见 test/）。
 */

const DEFAULTS = {
  BASE_PATH: '/api/incident',
  MAX_BYTES: 10 * 1024 * 1024, // 10 MiB
  RETENTION_DAYS: 30,
  RATE_LIMIT_MAX: 5, // 每个窗口允许的次数
  RATE_LIMIT_WINDOW_SECONDS: 3600, // 窗口 = 1 小时
  ALLOWED_TYPES: ['application/zip', 'application/octet-stream', 'application/x-zip-compressed'],
};

const ZIP_MAGIC = [0x50, 0x4b]; // "PK"
const ZIP_MAGIC_TAILS = [
  [0x03, 0x04], // 普通 zip
  [0x05, 0x06], // 空归档
  [0x07, 0x08], // 跨卷
];

/** 限流状态：**只在内存里**（每个 isolate 一份）。不写 R2、不写 KV。 */
const rateBuckets = new Map();

// --------------------------------------------------------------------- 工具

export function intEnv(env, key, fallback) {
  const raw = env && env[key];
  const n = raw === undefined || raw === null || raw === '' ? NaN : Number(raw);
  return Number.isFinite(n) && n > 0 ? Math.floor(n) : fallback;
}

export function basePath(env) {
  const raw = (env && env.BASE_PATH) || DEFAULTS.BASE_PATH;
  const trimmed = String(raw).replace(/\/+$/, '');
  return trimmed.startsWith('/') ? trimmed : `/${trimmed}`;
}

/** 常量时间比较（避免用 `===` 比 token 造成时序侧信道）。长度不同直接 false。 */
export function constantTimeEqual(a, b) {
  const x = new TextEncoder().encode(String(a ?? ''));
  const y = new TextEncoder().encode(String(b ?? ''));
  if (x.length === 0 || x.length !== y.length) return false;
  let diff = 0;
  for (let i = 0; i < x.length; i++) diff |= x[i] ^ y[i];
  return diff === 0;
}

export function makeId(nowMs = Date.now(), randomBytes = null) {
  const d = new Date(nowMs);
  const p = (n, w = 2) => String(n).padStart(w, '0');
  const stamp = `${d.getUTCFullYear()}${p(d.getUTCMonth() + 1)}${p(d.getUTCDate())}-` +
    `${p(d.getUTCHours())}${p(d.getUTCMinutes())}${p(d.getUTCSeconds())}`;
  let hex;
  if (randomBytes) {
    hex = Array.from(randomBytes.slice(0, 2)).map((b) => p(b.toString(16), 2)).join('');
  } else {
    const buf = new Uint8Array(2);
    crypto.getRandomValues(buf);
    hex = Array.from(buf).map((b) => p(b.toString(16), 2)).join('');
  }
  return `INC-${stamp}-${hex}`;
}

export const ID_RE = /^INC-\d{8}-\d{6}-[0-9a-f]{4}$/;

export async function sha256Hex(bytes) {
  const digest = await crypto.subtle.digest('SHA-256', bytes);
  return Array.from(new Uint8Array(digest)).map((b) => b.toString(16).padStart(2, '0')).join('');
}

/** 是否是 zip（按**魔数**判，不认 content-type 的自述）。 */
export function looksLikeZip(bytes) {
  if (!bytes || bytes.length < 4) return false;
  if (bytes[0] !== ZIP_MAGIC[0] || bytes[1] !== ZIP_MAGIC[1]) return false;
  return ZIP_MAGIC_TAILS.some((t) => bytes[2] === t[0] && bytes[3] === t[1]);
}

function json(obj, status, extra = {}) {
  return new Response(JSON.stringify(obj, null, 2), {
    status,
    headers: {
      'content-type': 'application/json; charset=utf-8',
      'cache-control': 'no-store',
      'x-robots-tag': 'noindex',
      ...extra,
    },
  });
}

/** 只说「发生了什么」，不回显 token、不回显 IP。`bodyExtra` 进 body，`headers` 进响应头。 */
function err(status, code, message, bodyExtra = {}, headers = {}) {
  return json({ error: code, message, ...bodyExtra }, status, headers);
}

// --------------------------------------------------------------------- 鉴权

function authorized(request, env) {
  const expected = (env && env.INCIDENT_TOKEN) || '';
  if (!expected) return false; // 没配 token ⇒ 一切特权操作都拒绝（fail closed）
  const got = request.headers.get('X-Auth-Token') || '';
  return constantTimeEqual(got, expected);
}

// --------------------------------------------------------------------- 限流

/**
 * 按 IP 限流。
 * **隐私**：键 = SHA-256(salt + ip) 的前 16 个 hex —— 内存里也不留原始 IP；
 * salt 每次 isolate 启动随机生成 ⇒ 跨 isolate 无法关联，重启即失效。
 * **诚实边界**：这是 best-effort（每个 isolate 一份内存），不是全局精确限流；
 * 需要强一致就得用 Durable Objects / KV，那超出本卡范围（写进 README）。
 */
const RATE_SALT = (() => {
  const b = new Uint8Array(16);
  crypto.getRandomValues(b);
  return Array.from(b).map((x) => x.toString(16).padStart(2, '0')).join('');
})();

export async function rateKey(ip) {
  return (await sha256Hex(new TextEncoder().encode(`${RATE_SALT}:${ip}`))).slice(0, 16);
}

export function clientIp(request) {
  return (
    request.headers.get('CF-Connecting-IP') ||
    (request.headers.get('X-Forwarded-For') || '').split(',')[0].trim() ||
    'unknown'
  );
}

/** 返回 {ok, retryAfter}；`nowMs` 可注入，测试里能精确构造窗口。 */
export async function checkRateLimit(request, env, nowMs = Date.now(), store = rateBuckets) {
  const max = intEnv(env, 'RATE_LIMIT_MAX', DEFAULTS.RATE_LIMIT_MAX);
  const windowS = intEnv(env, 'RATE_LIMIT_WINDOW_SECONDS', DEFAULTS.RATE_LIMIT_WINDOW_SECONDS);
  const key = await rateKey(clientIp(request));
  const bucket = store.get(key) || [];
  const cutoff = nowMs - windowS * 1000;
  const kept = bucket.filter((t) => t > cutoff);
  if (kept.length >= max) {
    store.set(key, kept);
    return { ok: false, retryAfter: Math.max(1, Math.ceil((kept[0] + windowS * 1000 - nowMs) / 1000)) };
  }
  kept.push(nowMs);
  store.set(key, kept);
  return { ok: true, retryAfter: 0 };
}

export function resetRateLimits(store = rateBuckets) {
  store.clear();
}

// --------------------------------------------------------------------- R2 键

const MANIFEST_KEY = (id) => `${id}/manifest.json`;
const BLOB_KEY = (id) => `${id}.zip`;

function expired(manifest, nowMs, env) {
  const days = intEnv(env, 'RETENTION_DAYS', DEFAULTS.RETENTION_DAYS);
  const received = Date.parse(manifest.received_at || '');
  if (!Number.isFinite(received)) return false;
  return nowMs - received > days * 86400 * 1000;
}

async function readManifest(env, id) {
  const obj = await env.INCIDENT_BUCKET.get(MANIFEST_KEY(id));
  if (!obj) return null;
  return JSON.parse(await obj.text());
}

// --------------------------------------------------------------------- 各接口

async function handleUpload(request, env) {
  // 1) 大小：先看声明，再核实际（两道）
  const max = intEnv(env, 'MAX_BYTES', DEFAULTS.MAX_BYTES);
  const declared = Number(request.headers.get('content-length') || '0');
  if (Number.isFinite(declared) && declared > max) {
    return err(413, 'too_large', `现场包上限 ${max} 字节（收到 ${declared}）`, { max_bytes: max });
  }

  // 2) 类型白名单：content-type 只是自述，**真正的判据是 zip 魔数**
  const ctype = (request.headers.get('content-type') || '').split(';')[0].trim().toLowerCase();
  const allowed = (env && env.ALLOWED_TYPES ? String(env.ALLOWED_TYPES).split(',') : DEFAULTS.ALLOWED_TYPES)
    .map((s) => s.trim().toLowerCase())
    .filter(Boolean);
  if (!allowed.includes(ctype)) {
    return err(415, 'bad_type', `只接受 ${allowed.join(' / ')}`, { allowed });
  }

  // 3) 限流（按 IP，内存 + 加盐哈希）
  const rl = await checkRateLimit(request, env);
  if (!rl.ok) {
    return err(429, 'rate_limited', `请求过于频繁：每 ${intEnv(env, 'RATE_LIMIT_WINDOW_SECONDS', DEFAULTS.RATE_LIMIT_WINDOW_SECONDS)} 秒最多 ${intEnv(env, 'RATE_LIMIT_MAX', DEFAULTS.RATE_LIMIT_MAX)} 次`, {
      retry_after_seconds: rl.retryAfter,
    }, { 'retry-after': String(rl.retryAfter) });
  }

  // 4) 读体 + 实际大小
  let bytes;
  try {
    bytes = new Uint8Array(await request.arrayBuffer());
  } catch {
    return err(400, 'bad_body', '读不到请求体');
  }
  if (bytes.byteLength > max) {
    return err(413, 'too_large', `现场包上限 ${max} 字节（实际 ${bytes.byteLength}）`, { max_bytes: max });
  }
  if (!looksLikeZip(bytes)) {
    return err(415, 'not_zip', '请求体不是 zip（魔数不符）');
  }

  // 5) 落盘：manifest + zip；**不写 IP**，客户端自述信息只在显式提供且格式合法时保留
  const nowMs = Date.now();
  const receivedAt = new Date(nowMs).toISOString();
  const id = makeId(nowMs);
  const sha = await sha256Hex(bytes);
  const selfDeclared = (request.headers.get('X-XrayTun-Client') || '').trim();
  const manifest = {
    schema_version: 1,
    id,
    sha256: sha,
    bytes: bytes.byteLength,
    content_type: ctype,
    received_at: receivedAt,
    retention_days: intEnv(env, 'RETENTION_DAYS', DEFAULTS.RETENTION_DAYS),
    ...(selfDeclared && /^[A-Za-z0-9._/+-]{1,64}$/.test(selfDeclared)
      ? { client_self_declared: selfDeclared }
      : {}),
  };
  await env.INCIDENT_BUCKET.put(MANIFEST_KEY(id), JSON.stringify(manifest, null, 2), {
    httpMetadata: { contentType: 'application/json; charset=utf-8' },
  });
  await env.INCIDENT_BUCKET.put(BLOB_KEY(id), bytes, {
    httpMetadata: { contentType: 'application/zip' },
  });

  return json({ id, sha256: sha, received_at: receivedAt, bytes: bytes.byteLength }, 201);
}

async function handleManifest(env, id) {
  const manifest = await readManifest(env, id);
  if (!manifest) return err(404, 'not_found', '没有这个 id');
  if (expired(manifest, Date.now(), env)) {
    // 惰性过期：即使 lifecycle 还没跑到，也不再把超期内容当作存在
    await env.INCIDENT_BUCKET.delete(MANIFEST_KEY(id));
    await env.INCIDENT_BUCKET.delete(BLOB_KEY(id));
    return err(404, 'expired', `已过保留期（${intEnv(env, 'RETENTION_DAYS', DEFAULTS.RETENTION_DAYS)} 天）`);
  }
  return json(manifest, 200);
}

async function handleBlob(request, env, id) {
  if (!authorized(request, env)) return err(401, 'unauthorized', '需要 X-Auth-Token');
  const manifest = await readManifest(env, id);
  if (!manifest) return err(404, 'not_found', '没有这个 id');
  if (expired(manifest, Date.now(), env)) {
    await env.INCIDENT_BUCKET.delete(MANIFEST_KEY(id));
    await env.INCIDENT_BUCKET.delete(BLOB_KEY(id));
    return err(404, 'expired', '已过保留期');
  }
  const obj = await env.INCIDENT_BUCKET.get(BLOB_KEY(id));
  if (!obj) return err(404, 'not_found', 'zip 不在了（可能被 lifecycle 清掉）');
  const body = await obj.arrayBuffer();
  return new Response(body, {
    status: 200,
    headers: {
      'content-type': 'application/zip',
      'content-disposition': `attachment; filename="${id}.zip"`,
      'cache-control': 'no-store',
      'x-robots-tag': 'noindex',
      'x-incident-sha256': manifest.sha256,
    },
  });
}

async function handleDelete(request, env, id) {
  if (!authorized(request, env)) return err(401, 'unauthorized', '需要 X-Auth-Token');
  const manifest = await readManifest(env, id);
  if (!manifest) return err(404, 'not_found', '没有这个 id');
  await env.INCIDENT_BUCKET.delete(MANIFEST_KEY(id));
  await env.INCIDENT_BUCKET.delete(BLOB_KEY(id));
  return json({ id, deleted: true, deleted_at: new Date().toISOString() }, 200);
}

// --------------------------------------------------------------------- 入口

export async function handle(request, env) {
  const url = new URL(request.url);
  const base = basePath(env);
  let path = url.pathname.replace(/\/+$/, '') || '/';

  if (path !== base && !path.startsWith(`${base}/`)) {
    return err(404, 'not_found', '路径不在本端点下', { base_path: base });
  }
  const rest = path === base ? '' : path.slice(base.length + 1);
  const parts = rest ? rest.split('/') : [];

  if (request.method === 'POST' && rest === '') return handleUpload(request, env);
  if (request.method === 'GET' && parts.length === 1) {
    if (!ID_RE.test(parts[0])) return err(400, 'bad_id', 'id 形如 INC-YYYYMMDD-HHMMSS-<4hex>');
    return handleManifest(env, parts[0]);
  }
  if (request.method === 'GET' && parts.length === 2 && parts[1] === 'blob') {
    if (!ID_RE.test(parts[0])) return err(400, 'bad_id', 'id 形如 INC-YYYYMMDD-HHMMSS-<4hex>');
    return handleBlob(request, env, parts[0]);
  }
  if (request.method === 'DELETE' && parts.length === 1) {
    if (!ID_RE.test(parts[0])) return err(400, 'bad_id', 'id 形如 INC-YYYYMMDD-HHMMSS-<4hex>');
    return handleDelete(request, env, parts[0]);
  }
  if (['POST', 'GET', 'DELETE'].includes(request.method)) {
    return err(404, 'not_found', '没有这个路由', { allowed: [`POST ${base}`, `GET ${base}/<id>`, `GET ${base}/<id>/blob`, `DELETE ${base}/<id>`] });
  }
  return err(405, 'method_not_allowed', `不支持 ${request.method}`);
}

export default {
  async fetch(request, env, ctx) {
    try {
      return await handle(request, env, ctx);
    } catch (e) {
      // 不把堆栈/内部细节回给调用方；也不把 token 或 IP 写进日志
      console.error('incident-endpoint error:', e && e.message ? e.message : 'unknown');
      return err(500, 'internal', '服务器内部错误');
    }
  },
};
