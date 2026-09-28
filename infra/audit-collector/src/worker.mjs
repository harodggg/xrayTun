/**
 * XrayTun 意图判定审计**密文**接收端点（Cloudflare Worker + R2）—— docs/design/AUDIT-SYNC.md §3/§4
 *
 * 设计口径（与 PRIVACY.md 一一对应）：
 *   · 收到的是**端到端加密后的密文信封**（ChaCha20-Poly1305）。服务端**只验形状、绝不解密** ——
 *     没有设备密钥，也永远不会有；分析在用户自己的机器上做（§7）。
 *   · 三个入口**全部 POST**：`/api/audit`（上传并覆盖当天）、`/api/audit/list`、`/api/audit/revoke`。
 *     为什么不做 GET/DELETE：设备侧 `crates/xt-intent/src/transport.rs` 只提供 `post`
 *     （刻意的：一个把域名发出去的功能不该顺手长出通用 HTTP 客户端），所以读/删也走 POST，
 *     不改传输层（契约 §4）。
 *   · 幂等：R2 key = `audit/<device>/<day>.json`，重传只覆盖同一个 key ⇒ 重试不可能产生重复数据。
 *   · **device 必须先过 `^[a-f0-9]{16}$` 才允许拼进 R2 key**：否则 `../` 之类能把对象写到别的前缀下
 *     （前缀穿越）。这一条是硬约束，见 `auditKey()` 里的第二道 self-guard。
 *   · **绝不回显 `ct`**：成功响应只有 key/replaced，错误信息里也只说「哪一项不合法」，不带值。
 *   · **不把客户端 IP 写进 R2**：限流键 = 加盐哈希的 IP，只在 isolate 内存里，不进持久层；
 *     不采集请求日志（`[observability] enabled = false`）。
 *
 * 无任何第三方依赖：只用 Web 标准 API + R2 绑定，测试用 node --test（见 test/）。
 */

const DEFAULTS = {
  BASE_PATH: '/api/audit',
  MAX_BYTES: 10 * 1024 * 1024, // 10 MiB（契约 §4 的硬上限）
  RETENTION_DAYS: 400, // 与 R2 lifecycle 对齐（见 README「R2 lifecycle」）
  RATE_LIMIT_MAX: 120, // 每个窗口允许的次数
  RATE_LIMIT_WINDOW_SECONDS: 3600, // 窗口 = 1 小时
};

/**
 * 契约 §3：线上唯一的信封算法标识。服务端只比较字符串，不理解密码学。
 *
 * ⚠️ **不要 `export` 这个字符串常量**：workerd 会把入口模块的每个命名导出都当成候选
 * handler/entrypoint，字符串不是 `function or ExportedHandler` ⇒ 整个 Worker **起不来**：
 *   `Incorrect type for map entry 'ALG': the provided value is not of type 'function or ExportedHandler'`
 * 这是 `smoke.sh`（真 workerd）抓到的，`node --test` 全绿也发现不了 —— 又一个「本地绿 ≠ 运行时绿」。
 * 导出的正则（DEVICE_RE 等）没问题（incident-collector 的 ID_RE 已在线验证）。
 */
const ALG = 'chacha20poly1305';

// 形状正则（契约 §4）。**注意 direction**：这些是「允许拼 key / 允许落盘」的唯一判据，
// 所以只接受**小写** hex —— 大写会让同一个设备产生两个不同前缀，破坏「按设备分组/撤回」。
export const DEVICE_RE = /^[a-f0-9]{16}$/;
export const DAY_RE = /^\d{4}-\d{2}-\d{2}$/;
export const NONCE_RE = /^[a-f0-9]{24}$/; // 24 hex = 12 字节
export const CT_RE = /^[a-f0-9]+$/;

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

export async function sha256Hex(bytes) {
  const digest = await crypto.subtle.digest('SHA-256', bytes);
  return Array.from(new Uint8Array(digest)).map((b) => b.toString(16).padStart(2, '0')).join('');
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

/** 只说「发生了什么」，不回显 token、不回显 IP、**不回显 ct**。 */
function err(status, code, message, bodyExtra = {}, headers = {}) {
  return json({ error: code, message, ...bodyExtra }, status, headers);
}

// --------------------------------------------------------------------- 鉴权

/** 只认 `Authorization: Bearer <token>`（契约 §4）。其它头名/方案一律视为没带。 */
export function bearerToken(request) {
  const raw = request.headers.get('authorization') || '';
  const m = /^Bearer[ \t]+(.+)$/i.exec(raw.trim());
  return m ? m[1].trim() : '';
}

function authorized(request, env) {
  const expected = (env && env.AUDIT_TOKEN) || '';
  if (!expected) return false; // 没配 token ⇒ 一切操作都拒绝（fail closed）
  return constantTimeEqual(bearerToken(request), expected);
}

// --------------------------------------------------------------------- 限流

/**
 * 按 IP 限流。
 * **隐私**：键 = SHA-256(salt + ip) 的前 16 个 hex —— 内存里也不留原始 IP；
 * salt 每次 isolate 启动随机生成 ⇒ 跨 isolate 无法关联，重启即失效。
 * **诚实边界**：这是 best-effort（每个 isolate 一份内存），不是全局精确限流、**不是授权判据**
 * —— 真正的门是 token 鉴权 + 形状白名单 + 大小上限 + 400 天保留（见 README §5、PRIVACY.md）。
 */
// ⚠️ 盐**不能在模块顶层生成**：workerd 禁止在 global scope 里做「异步 I/O / 设超时 / 取随机值」。
// 2026-09-22 incident-collector 的真实部署事故就是这么被拒的：
//   Uncaught Error: Disallowed operation called within global scope.  … [code: 10021]
// （`node --test` 不执行这条运行时限制 ⇒ 本地全绿也抓不到它；所以另有 `smoke.sh` 用真 workerd 冒烟。）
// 设计意图不变：盐只在**本 isolate 的内存**里、首次用到时惰性生成、isolate 重启即失效。
let rateSalt = null;

function getRateSalt() {
  if (rateSalt === null) {
    const b = new Uint8Array(16);
    crypto.getRandomValues(b);
    rateSalt = Array.from(b).map((x) => x.toString(16).padStart(2, '0')).join('');
  }
  return rateSalt;
}

export async function rateKey(ip) {
  return (await sha256Hex(new TextEncoder().encode(`${getRateSalt()}:${ip}`))).slice(0, 16);
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

export function rateLimited(env, rl) {
  return err(
    429,
    'rate_limited',
    `请求过于频繁：每 ${intEnv(env, 'RATE_LIMIT_WINDOW_SECONDS', DEFAULTS.RATE_LIMIT_WINDOW_SECONDS)} 秒最多 ${intEnv(env, 'RATE_LIMIT_MAX', DEFAULTS.RATE_LIMIT_MAX)} 次`,
    { retry_after_seconds: rl.retryAfter },
    { 'retry-after': String(rl.retryAfter) },
  );
}

// --------------------------------------------------------------------- R2 键

/**
 * 拼 R2 key。**device / day 已在调用前过正则**，这里再守一道：
 * 这个函数被误用到未校验的输入上时必须**抛错**，而不是拼出一个带 `../` 的 key。
 * （前缀穿越：`audit/../x/…` 会把对象写到 `x/` 前缀下，撤回时也删不干净。）
 */
export function auditKey(device, day) {
  if (!DEVICE_RE.test(String(device))) throw new Error('device 未过 ^[a-f0-9]{16}$：拒绝拼 R2 key');
  if (!DAY_RE.test(String(day))) throw new Error('day 未过 ^\\d{4}-\\d{2}-\\d{2}$：拒绝拼 R2 key');
  return `audit/${device}/${day}.json`;
}

/** 某个 device 的对象前缀。device 必须先过正则（list/revoke 都用它）。 */
export function devicePrefix(device) {
  if (!DEVICE_RE.test(String(device))) throw new Error('device 未过 ^[a-f0-9]{16}$：拒绝拼前缀');
  return `audit/${device}/`;
}

/** 从 key 反推 day（`audit/<device>/<day>.json`）。形状不符就返回 null —— 不编造。 */
export function dayFromKey(key, device) {
  const prefix = devicePrefix(device);
  if (typeof key !== 'string' || !key.startsWith(prefix) || !key.endsWith('.json')) return null;
  const day = key.slice(prefix.length, -'.json'.length);
  return DAY_RE.test(day) ? day : null;
}

/**
 * R2 对象的 `uploaded` ⇒ unix 秒。**只认 Date**（真 R2 的 R2Object.uploaded 就是 Date）；
 * 拿不到（替身没给 / 形状不对）就返回 null —— 契约要求「拿不到就 null，别编」。
 */
export function uploadedUnix(obj) {
  const u = obj && obj.uploaded;
  if (u instanceof Date && Number.isFinite(u.getTime())) return Math.floor(u.getTime() / 1000);
  return null;
}

/**
 * 分页 list 出某前缀下的**全部**对象。
 * 为什么要单独抽出来：R2 的 list 一次最多回 1000 条，`truncated=true` 时**必须**带 cursor 再列一次。
 * revoke 只删第一页 = 看起来成功但没删干净（禁止的实现）；list 只读第一页 = 静默丢数据。
 * 这里同时防两种退化：truncated 却没给 cursor / cursor 不前进 ⇒ **抛错**（500），
 * 而不是返回一个「短了但不说」的结果。
 */
export async function listAll(env, prefix) {
  const out = [];
  const seenCursors = new Set();
  let cursor;
  for (;;) {
    const page = await env.AUDIT_BUCKET.list(cursor ? { prefix, cursor } : { prefix });
    const objects = (page && page.objects) || [];
    for (const o of objects) {
      // 再守一道：只接受确实落在本前缀下的 key（即便 binding 行为异常也不误删别人的数据）
      if (o && typeof o.key === 'string' && o.key.startsWith(prefix)) out.push(o);
    }
    if (!page || page.truncated !== true) break;
    const next = page.cursor;
    if (!next) throw new Error('R2 list 返回 truncated 但没有 cursor：拒绝把不完整的结果当作完整');
    if (seenCursors.has(next)) throw new Error('R2 list cursor 没有前进：拒绝死循环');
    seenCursors.add(next);
    cursor = next;
  }
  return out;
}

// --------------------------------------------------------------------- 形状校验（绝不解密）

/**
 * 只验**形状**，不碰密码学。返回 {ok:true} 或 {ok:false, field, message}。
 * **message 里绝不带字段值**（尤其 `ct`）—— 否则响应体本身成了泄漏面。
 */
export function validateEnvelope(body) {
  if (body === null || typeof body !== 'object' || Array.isArray(body)) {
    return { ok: false, field: 'body', message: 'body 必须是 JSON 对象（密文信封）' };
  }
  if (body.v !== 1) return { ok: false, field: 'v', message: 'v 必须是数字 1（当前只有 v1 信封）' };
  if (body.alg !== ALG) return { ok: false, field: 'alg', message: `alg 必须是 "${ALG}"` };
  if (typeof body.device !== 'string' || !DEVICE_RE.test(body.device)) {
    return { ok: false, field: 'device', message: 'device 必须是 16 位小写 hex（8 字节随机 id）' };
  }
  if (typeof body.day !== 'string' || !DAY_RE.test(body.day)) {
    return { ok: false, field: 'day', message: 'day 必须是 YYYY-MM-DD' };
  }
  if (typeof body.nonce !== 'string' || !NONCE_RE.test(body.nonce)) {
    return { ok: false, field: 'nonce', message: 'nonce 必须是 24 位小写 hex（12 字节）' };
  }
  if (typeof body.ct !== 'string' || !CT_RE.test(body.ct)) {
    return { ok: false, field: 'ct', message: 'ct 必须是非空小写 hex 字符串（不回显内容）' };
  }
  // rows/bytes 是明文头里的元数据（§3）：list/revoke 要用它，且它们**不参与认证**
  // ⇒ 服务端只检查「是不是非负整数」，绝不据此做任何安全判断。
  if (!Number.isInteger(body.rows) || body.rows < 0) {
    return { ok: false, field: 'rows', message: 'rows 必须是非负整数' };
  }
  if (!Number.isInteger(body.bytes) || body.bytes < 0) {
    return { ok: false, field: 'bytes', message: 'bytes 必须是非负整数（明文长度的估算，仅人读）' };
  }
  return { ok: true };
}

/**
 * 读 body：**先看 Content-Length 再读**（契约 §4）。返回
 * {ok:true, raw:Uint8Array} / {ok:false, res:Response}。
 * `declared` 缺失或不可解析时不当成 0 —— 退化为「读出来再核实际大小」。
 */
async function readBody(request, env, max) {
  const declared = Number(request.headers.get('content-length') || '0');
  if (Number.isFinite(declared) && declared > max) {
    return { ok: false, res: err(413, 'too_large', `请求体上限 ${max} 字节（声明 ${declared}）`, { max_bytes: max }) };
  }
  let raw;
  try {
    raw = new Uint8Array(await request.arrayBuffer());
  } catch {
    return { ok: false, res: err(400, 'bad_body', '读不到请求体') };
  }
  if (raw.byteLength > max) {
    return { ok: false, res: err(413, 'too_large', `请求体上限 ${max} 字节（实际 ${raw.byteLength}）`, { max_bytes: max }) };
  }
  return { ok: true, raw };
}

/** 解析 JSON 对象（小 body：list/revoke）。 */
function parseJsonObject(raw) {
  let text;
  try {
    text = new TextDecoder('utf-8', { fatal: false }).decode(raw);
  } catch {
    return { ok: false };
  }
  let body;
  try {
    body = JSON.parse(text);
  } catch {
    return { ok: false };
  }
  if (body === null || typeof body !== 'object' || Array.isArray(body)) return { ok: false };
  return { ok: true, body };
}

/** 三个入口公共的前置：鉴权 → Content-Length → 限流。 */
async function guard(request, env) {
  if (!authorized(request, env)) {
    return { ok: false, res: err(401, 'unauthorized', '需要 Authorization: Bearer <AUDIT_TOKEN>') };
  }
  const max = intEnv(env, 'MAX_BYTES', DEFAULTS.MAX_BYTES);
  const declared = Number(request.headers.get('content-length') || '0');
  if (Number.isFinite(declared) && declared > max) {
    return { ok: false, res: err(413, 'too_large', `请求体上限 ${max} 字节（声明 ${declared}）`, { max_bytes: max }) };
  }
  const rl = await checkRateLimit(request, env);
  if (!rl.ok) return { ok: false, res: rateLimited(env, rl) };
  return { ok: true, max };
}

// --------------------------------------------------------------------- 各接口

/** POST /api/audit —— 上传（覆盖同 key），返回 {ok,key,replaced}。绝不回显 ct。 */
async function handleUpload(request, env) {
  const g = await guard(request, env);
  if (!g.ok) return g.res;

  const rb = await readBody(request, env, g.max);
  if (!rb.ok) return rb.res;

  let body;
  try {
    body = JSON.parse(new TextDecoder('utf-8', { fatal: false }).decode(rb.raw));
  } catch {
    return err(400, 'bad_json', 'body 不是合法 JSON');
  }

  const v = validateEnvelope(body);
  if (!v.ok) return err(400, 'bad_envelope', `密文信封形状不对：${v.message}`, { field: v.field });

  // device/day 已经过正则 ⇒ auditKey 内部的 self-guard 不会触发；拼出来的 key 精确等于
  // audit/<device>/<day>.json（契约 §4）。
  const key = auditKey(body.device, body.day);

  // replaced 语义：**写之前**先 head，命中已有对象才是 true（不是「写成功就算 true」）。
  const existing = await env.AUDIT_BUCKET.head(key);
  const replaced = existing !== null && existing !== undefined;

  // 原样存**收到的字节**（不重新序列化）：设备侧「同一天必须逐字节相同」的确定性因此可以
  // 一直追溯到密文本身；重传 = 覆盖同一个 key（幂等）。
  await env.AUDIT_BUCKET.put(key, rb.raw, {
    httpMetadata: { contentType: 'application/json; charset=utf-8' },
  });

  // ⚠️ 响应里只有 key 与 replaced：**没有 ct、没有 nonce、没有 body 回显**。
  return json({ ok: true, key, replaced }, 200);
}

/** POST /api/audit/list —— 列出该 device 的元数据。 */
async function handleList(request, env) {
  const g = await guard(request, env);
  if (!g.ok) return g.res;

  const rb = await readBody(request, env, g.max);
  if (!rb.ok) return rb.res;
  const pj = parseJsonObject(rb.raw);
  if (!pj.ok) return err(400, 'bad_body', 'body 必须是 JSON 对象 {"device":"<16hex>"}');
  const device = pj.body.device;
  if (typeof device !== 'string' || !DEVICE_RE.test(device)) {
    return err(400, 'bad_device', 'device 必须是 16 位小写 hex', { field: 'device' });
  }

  const prefix = devicePrefix(device);
  const retentionDays = intEnv(env, 'RETENTION_DAYS', DEFAULTS.RETENTION_DAYS);
  const nowMs = Date.now();
  const objects = await listAll(env, prefix);

  const items = [];
  for (const o of objects) {
    const uploaded = uploadedUnix(o);
    // 惰性过期（**兜底**，主机制是 R2 lifecycle，见 README）：只在 uploaded 拿得到时判。
    // 为什么要有：lifecycle 规则曾经「事实上不存在」过（见 incident-collector 的教训），
    // 读路径上多一道就不必只依赖它。判据与 lifecycle 同义（对象年龄 > RETENTION_DAYS）。
    if (uploaded !== null && nowMs - uploaded * 1000 > retentionDays * 86400 * 1000) {
      await env.AUDIT_BUCKET.delete(o.key);
      continue;
    }
    let rows = null;
    let bytes = null;
    try {
      const obj = await env.AUDIT_BUCKET.get(o.key);
      if (obj) {
        const stored = JSON.parse(await obj.text());
        if (Number.isInteger(stored.rows) && stored.rows >= 0) rows = stored.rows;
        if (Number.isInteger(stored.bytes) && stored.bytes >= 0) bytes = stored.bytes;
      }
    } catch {
      // 对象读不出/坏 JSON：rows/bytes 留 null —— **不编造**（契约：拿不到就 null）
    }
    items.push({
      day: dayFromKey(o.key, device),
      rows,
      bytes,
      key: o.key,
      uploaded_unix: uploaded,
    });
  }
  items.sort((a, b) => String(a.day).localeCompare(String(b.day)) || String(a.key).localeCompare(String(b.key)));
  return json({ ok: true, items }, 200);
}

/** POST /api/audit/revoke —— 删掉该 device 前缀下的**全部**对象（分页删到底）。 */
async function handleRevoke(request, env) {
  const g = await guard(request, env);
  if (!g.ok) return g.res;

  const rb = await readBody(request, env, g.max);
  if (!rb.ok) return rb.res;
  const pj = parseJsonObject(rb.raw);
  if (!pj.ok) return err(400, 'bad_body', 'body 必须是 JSON 对象 {"device":"<16hex>"}');
  const device = pj.body.device;
  if (typeof device !== 'string' || !DEVICE_RE.test(device)) {
    return err(400, 'bad_device', 'device 必须是 16 位小写 hex', { field: 'device' });
  }

  const prefix = devicePrefix(device);
  const objects = await listAll(env, prefix); // 分页列完再删 ⇒ 不会只删第一页
  let deleted = 0;
  for (const o of objects) {
    await env.AUDIT_BUCKET.delete(o.key);
    deleted++;
  }
  return json({ ok: true, deleted }, 200);
}

// --------------------------------------------------------------------- 入口

export async function handle(request, env) {
  const url = new URL(request.url);
  const base = basePath(env);
  const path = url.pathname.replace(/\/+$/, '') || '/';

  if (path !== base && !path.startsWith(`${base}/`)) {
    return err(404, 'not_found', '路径不在本端点下', { base_path: base });
  }
  const rest = path === base ? '' : path.slice(base.length + 1);
  const parts = rest ? rest.split('/') : [];
  const isPost = request.method === 'POST';
  const allowed = [`POST ${base}`, `POST ${base}/list`, `POST ${base}/revoke`];

  if (rest === '') {
    return isPost ? handleUpload(request, env) : err(405, 'method_not_allowed', `不支持 ${request.method}（上传入口只接受 POST）`, { allowed });
  }
  if (parts.length === 1 && (parts[0] === 'list' || parts[0] === 'revoke')) {
    if (!isPost) return err(405, 'method_not_allowed', `不支持 ${request.method}（只接受 POST）`, { allowed });
    return parts[0] === 'list' ? handleList(request, env) : handleRevoke(request, env);
  }
  // 路径不对 —— 注意这与 405 不同：405 说明路由对、方法错。
  return err(404, 'not_found', '没有这个路由', { allowed });
}

export default {
  async fetch(request, env, ctx) {
    try {
      return await handle(request, env, ctx);
    } catch (e) {
      // 不把堆栈/内部细节回给调用方；也不把 token、IP 或 ct 写进日志
      console.error('audit-endpoint error:', e && e.message ? e.message : 'unknown');
      return err(500, 'internal', '服务器内部错误');
    }
  },
};
