/**
 * 审计密文端点的本地自测（**不需要 Cloudflare 账号**）：`node --test test/worker.test.mjs`
 *
 * 覆盖 docs/design/AUDIT-SYNC.md §3/§4 点名的每个分支：
 *   鉴权（401）/ 信封形状（v/alg/device/day/nonce/ct/rows/bytes，400）/ **device 前缀穿越** /
 *   大小上限（先看 Content-Length、再看实际）/ 限流（429）/ R2 key 精确形状 / replaced 语义 /
 *   list 元数据与**分页** / revoke 只删本 device 前缀且**分页删完** / 响应不回显 ct / 路由与 404。
 *
 * `AUDIT_MODULE` 环境变量可指向**另一份** worker 源码 —— `verify.sh --sensitivity`
 * 就是用它把「鉴权被拿掉」「device 正则被拿掉」的变体喂进来，证明这些用例**真的在验**它们。
 */
import test from 'node:test';
import assert from 'node:assert/strict';

const MOD = process.env.AUDIT_MODULE || new URL('../src/worker.mjs', import.meta.url).href;
const worker = await import(MOD);

// 每个用例都从干净的限流状态开始：限流是**模块级内存**，不 reset 会串味
// （incident-collector 第一版就踩了：默认配额被前面的用例吃掉，后面拿到 429 而不是各自要验的状态码）
test.beforeEach(() => worker.resetRateLimits());

const BASE = 'https://xraytun.top/api/audit';
const TOKEN = 'test-token-do-not-use';
const IP = '203.0.113.7';
const DEV = '3f2a91c4d0be7715';
const DAY = '2026-09-24';

// --- 有辨识度的密文字段：用来断言**响应体里绝不出现它们** ---
const CT = 'cafe'.repeat(16); // 64 hex
const NONCE = 'abcdefabcdefabcdefabcdef'; // 24 hex

// ------------------------------------------------------------------ 测试替身

/**
 * 假 R2。支持 worker 用到的 head / get / put / delete / list。
 * `pageLimit` 用来把 list 逼成多页 —— 分页是本题的必测点（revoke 只删第一页 = 假成功）。
 * `uploaded` 是真 R2 的 R2Object.uploaded 语义（Date）；替身必须给，否则测不出 uploaded_unix。
 */
function fakeR2({ pageLimit = 1000 } = {}) {
  const map = new Map(); // key -> { value: Uint8Array, opts, uploaded: Date|undefined }
  let listCalls = 0;
  return {
    map,
    get listCalls() {
      return listCalls;
    },
    async put(key, value, opts) {
      map.set(key, { value, opts, uploaded: new Date() });
    },
    async head(key) {
      const e = map.get(key);
      if (!e) return null;
      return { key, size: e.value.byteLength, uploaded: e.uploaded };
    },
    async get(key) {
      const e = map.get(key);
      if (!e) return null;
      const bytes = e.value instanceof Uint8Array ? e.value : new TextEncoder().encode(String(e.value));
      return {
        key,
        size: bytes.byteLength,
        uploaded: e.uploaded,
        httpMetadata: e.opts && e.opts.httpMetadata,
        async text() {
          return new TextDecoder().decode(bytes);
        },
        async arrayBuffer() {
          return bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength);
        },
      };
    },
    async delete(key) {
      map.delete(key);
    },
    async list({ prefix = '', cursor } = {}) {
      listCalls++;
      const keys = [...map.keys()].filter((k) => k.startsWith(prefix)).sort();
      let start = 0;
      if (cursor) {
        const i = keys.indexOf(cursor);
        start = i < 0 ? keys.length : i;
      }
      const slice = keys.slice(start, start + pageLimit);
      const truncated = start + pageLimit < keys.length;
      return {
        objects: slice.map((k) => ({ key: k, size: map.get(k).value.byteLength, uploaded: map.get(k).uploaded })),
        truncated,
        cursor: truncated ? keys[start + pageLimit] : undefined,
      };
    },
  };
}

function env(extra = {}) {
  return { AUDIT_BUCKET: fakeR2(), AUDIT_TOKEN: TOKEN, ...extra };
}

/** 直接往假 R2 里塞一个对象（用于构造「uploaded 拿不到」「已过期」等边界）。 */
function seed(bucket, device, day, envelope, uploaded) {
  bucket.map.set(`audit/${device}/${day}.json`, {
    value: new TextEncoder().encode(JSON.stringify(envelope)),
    opts: {},
    uploaded,
  });
}

// ------------------------------------------------------------------ 请求构造

function envelope(over = {}) {
  return {
    v: 1,
    alg: 'chacha20poly1305',
    device: DEV,
    day: DAY,
    rows: 123,
    bytes: 45678,
    nonce: NONCE,
    ct: CT,
    ...over,
  };
}

function req(path = '', { token = TOKEN, ip = IP, method = 'POST', raw, obj, headers = {} } = {}) {
  const h = { 'content-type': 'application/json', 'CF-Connecting-IP': ip, ...headers };
  if (token !== null) h.authorization = `Bearer ${token}`;
  const init = { method, headers: h };
  if (raw !== undefined) init.body = raw;
  else if (obj !== undefined) init.body = JSON.stringify(obj);
  return new Request(BASE + path, init);
}

async function call(path, envObj, opts = {}) {
  const res = await worker.handle(req(path, opts), envObj);
  const text = await res.text();
  let body = null;
  try {
    body = JSON.parse(text);
  } catch {
    /* 非 JSON 响应（不应发生）保持 null */
  }
  return { res, text, body };
}

const upload = (envObj, envlp, opts = {}) => call('', envObj, { obj: envlp, ...opts });

// ------------------------------------------------------------------ 鉴权

test('鉴权：三个入口不带 token ⇒ 401，且什么都不写', async () => {
  const e = env();
  const up = await upload(e, envelope(), { token: null });
  assert.equal(up.res.status, 401);
  assert.equal(up.body.error, 'unauthorized');
  const ls = await call('/list', e, { token: null, obj: { device: DEV } });
  assert.equal(ls.res.status, 401);
  const rv = await call('/revoke', e, { token: null, obj: { device: DEV } });
  assert.equal(rv.res.status, 401);
  assert.equal(e.AUDIT_BUCKET.map.size, 0, '未授权请求绝不能落盘');
});

test('鉴权：错 token ⇒ 401；`X-Auth-Token` 不算数（只认 Bearer）', async () => {
  const e = env();
  const wrong = await upload(e, envelope(), { token: 'wrong' });
  assert.equal(wrong.res.status, 401);
  // incident-collector 用的是 X-Auth-Token；本端点只认 Authorization: Bearer —— 传错头名必须 401
  const otherHeader = await upload(e, envelope(), { token: null, headers: { 'X-Auth-Token': TOKEN } });
  assert.equal(otherHeader.res.status, 401);
  assert.equal(e.AUDIT_BUCKET.map.size, 0);
});

test('鉴权：没配置 AUDIT_TOKEN ⇒ 一律 401（fail closed）', async () => {
  const e = env({ AUDIT_TOKEN: '' });
  const up = await upload(e, envelope());
  assert.equal(up.res.status, 401);
  const ls = await call('/list', e, { obj: { device: DEV } });
  assert.equal(ls.res.status, 401);
});

test('鉴权：对 token ⇒ 正常放行', async () => {
  const e = env();
  const { res, body } = await upload(e, envelope());
  assert.equal(res.status, 200);
  assert.equal(body.ok, true);
});

// ------------------------------------------------------------------ 信封形状（400）

test('形状：v 不是 1 ⇒ 400，field=v，且不落盘', async () => {
  const e = env();
  const { res, body } = await upload(e, envelope({ v: 2 }));
  assert.equal(res.status, 400);
  assert.equal(body.error, 'bad_envelope');
  assert.equal(body.field, 'v');
  assert.equal(e.AUDIT_BUCKET.map.size, 0);
});

test('形状：alg 不是 chacha20poly1305 ⇒ 400，field=alg', async () => {
  const e = env();
  const { res, body } = await upload(e, envelope({ alg: 'aes-gcm' }));
  assert.equal(res.status, 400);
  assert.equal(body.field, 'alg');
});

test('形状：day 不是 YYYY-MM-DD ⇒ 400，field=day', async () => {
  const e = env();
  const { res, body } = await upload(e, envelope({ day: '2026-9-4' }));
  assert.equal(res.status, 400);
  assert.equal(body.field, 'day');
});

test('形状：nonce 不是 24 位 hex ⇒ 400，field=nonce', async () => {
  const e = env();
  const { res, body } = await upload(e, envelope({ nonce: 'ab'.repeat(11) }));
  assert.equal(res.status, 400);
  assert.equal(body.field, 'nonce');
});

test('形状：ct 不是非空小写 hex ⇒ 400，field=ct，且**不回显 ct**', async () => {
  const e = env();
  const bad = 'ZZZZ-not-hex-NEEDLE';
  const { res, text, body } = await upload(e, envelope({ ct: bad }));
  assert.equal(res.status, 400);
  assert.equal(body.field, 'ct');
  assert.ok(!text.includes('ZZZZ'), '响应体里不许出现 ct 原文（哪怕它不合法）');
  assert.ok(!text.includes('NEEDLE'), '响应体里不许出现 ct 原文（哪怕它不合法）');
  assert.equal(e.AUDIT_BUCKET.map.size, 0);
});

test('形状：ct 为空串 ⇒ 400（`+` 要求至少一个字符）', async () => {
  const e = env();
  const { res, body } = await upload(e, envelope({ ct: '' }));
  assert.equal(res.status, 400);
  assert.equal(body.field, 'ct');
});

test('形状：rows / bytes 必须是非负整数 ⇒ 400', async () => {
  const e = env();
  const a = await upload(e, envelope({ rows: -1 }));
  assert.equal(a.res.status, 400);
  assert.equal(a.body.field, 'rows');
  const b = await upload(e, envelope({ bytes: 'x' }));
  assert.equal(b.res.status, 400);
  assert.equal(b.body.field, 'bytes');
  const c = envelope();
  delete c.rows;
  const d = await upload(e, c);
  assert.equal(d.res.status, 400);
  assert.equal(d.body.field, 'rows');
});

test('形状：body 不是合法 JSON / 不是对象 ⇒ 400', async () => {
  const e = env();
  const badJson = await upload(e, undefined, { raw: '{not json' });
  assert.equal(badJson.res.status, 400);
  assert.equal(badJson.body.error, 'bad_json');
  const arr = await upload(e, undefined, { raw: '[1,2]' });
  assert.equal(arr.res.status, 400);
  assert.equal(arr.body.error, 'bad_envelope');
  const nul = await upload(e, undefined, { raw: 'null' });
  assert.equal(nul.res.status, 400);
  assert.equal(nul.body.field, 'body');
});

// ------------------------------------------------------------------ device 前缀穿越（核心安全点）

test('device 没过多正则 ⇒ 400，且**绝不拼 key / 绝不落盘**', async () => {
  const bads = [
    '../x',
    '../../etc/passwd',
    '',
    'a'.repeat(17), // 超长
    '3f2a91c4d0be771', // 15 位（短）
    '3F2A91C4D0BE7715', // 大写 hex（会让同一设备产生两个前缀）
    'zzzz91c4d0be7715', // 非 hex
    '3f2a91c4d0be7715/extra', // 带斜杠
    '*',
  ];
  for (const device of bads) {
    const e = env();
    const { res, body } = await upload(e, envelope({ device }));
    assert.equal(res.status, 400, `device=${JSON.stringify(device)} 应被拒`);
    assert.equal(body.field ?? body.error, 'device', `device=${JSON.stringify(device)}`);
    assert.equal(e.AUDIT_BUCKET.map.size, 0, `device=${JSON.stringify(device)} 不该落盘`);
  }
});

test('auditKey / devicePrefix 对非法输入**抛错**（第二道 self-guard）', () => {
  assert.throws(() => worker.auditKey('../x', DAY));
  assert.throws(() => worker.auditKey(DEV, '../x'));
  assert.throws(() => worker.devicePrefix('..'));
  assert.equal(worker.auditKey(DEV, DAY), `audit/${DEV}/${DAY}.json`);
  assert.equal(worker.dayFromKey(`audit/${DEV}/${DAY}.json`, DEV), DAY);
  assert.equal(worker.dayFromKey(`audit/${DEV}/../evil.json`, DEV), null);
});

// ------------------------------------------------------------------ 大小上限

test('大小：Content-Length 超限 ⇒ 413，且**根本不读 body**（顺序即证据）', async () => {
  const e = env({ MAX_BYTES: '1024' });
  let read = false;
  const stub = {
    method: 'POST',
    url: BASE,
    headers: {
      get: (k) => {
        const key = String(k).toLowerCase();
        if (key === 'authorization') return `Bearer ${TOKEN}`;
        if (key === 'content-length') return String(64 * 1024 * 1024);
        return null;
      },
    },
    async arrayBuffer() {
      read = true;
      return new ArrayBuffer(0);
    },
  };
  const res = await worker.handle(stub, e);
  assert.equal(res.status, 413);
  assert.equal((await res.json()).error, 'too_large');
  assert.equal(read, false, '声明超限时必须先返回 413，不能去读 body');
});

test('大小：没有 Content-Length 但实际超限 ⇒ 413（第二道门）', async () => {
  const e = env({ MAX_BYTES: '1024' });
  const big = new Uint8Array(2048).fill(0x41);
  const stub = {
    method: 'POST',
    url: BASE,
    headers: {
      get: (k) => (String(k).toLowerCase() === 'authorization' ? `Bearer ${TOKEN}` : null),
    },
    async arrayBuffer() {
      return big.buffer;
    },
  };
  const res = await worker.handle(stub, e);
  assert.equal(res.status, 413);
});

test('大小：正常信封（含 10 MiB 默认上限）不受影响', async () => {
  const e = env();
  const { res } = await upload(e, envelope());
  assert.equal(res.status, 200);
});

// ------------------------------------------------------------------ 限速

test('限速：第 3 次 ⇒ 429 + Retry-After；换 IP 不受影响', async () => {
  const e = env({ RATE_LIMIT_MAX: '2', RATE_LIMIT_WINDOW_SECONDS: '3600' });
  await upload(e, envelope(), { ip: '198.51.100.9' });
  await upload(e, envelope(), { ip: '198.51.100.9' });
  const { res, body } = await upload(e, envelope(), { ip: '198.51.100.9' });
  assert.equal(res.status, 429);
  assert.equal(body.error, 'rate_limited');
  assert.ok(body.retry_after_seconds >= 1);
  assert.equal(res.headers.get('retry-after'), String(body.retry_after_seconds));
  const other = await upload(e, envelope(), { ip: '198.51.100.10' });
  assert.equal(other.res.status, 200);
});

test('限速：list / revoke 同样受限（契约 §4 的失败列里有 429）', async () => {
  const e = env({ RATE_LIMIT_MAX: '1', RATE_LIMIT_WINDOW_SECONDS: '3600' });
  const a = await call('/list', e, { obj: { device: DEV }, ip: '198.51.100.30' });
  assert.equal(a.res.status, 200);
  const b = await call('/list', e, { obj: { device: DEV }, ip: '198.51.100.30' });
  assert.equal(b.res.status, 429);
  const c = await call('/revoke', e, { obj: { device: DEV }, ip: '198.51.100.31' });
  const d = await call('/revoke', e, { obj: { device: DEV }, ip: '198.51.100.31' });
  assert.equal(c.res.status, 200);
  assert.equal(d.res.status, 429);
});

// ------------------------------------------------------------------ R2 key / replaced

test('落盘：key 精确等于 audit/<device>/<day>.json，且原样存密文字节', async () => {
  const e = env();
  const { res, body } = await upload(e, envelope());
  assert.equal(res.status, 200);
  const key = `audit/${DEV}/${DAY}.json`;
  assert.equal(body.key, key);
  assert.equal(body.replaced, false);
  assert.ok(e.AUDIT_BUCKET.map.has(key), `R2 里应有 ${key}`);
  assert.deepEqual([...e.AUDIT_BUCKET.map.keys()], [key]);
  // 存的必须是**收到的字节**（内容确定性可追溯），不是重新序列化的结果
  const stored = new TextDecoder().decode(e.AUDIT_BUCKET.map.get(key).value);
  assert.deepEqual(JSON.parse(stored), envelope());
});

test('replaced 语义：首次 false，同 key 重传 true，且对象数不增加、内容被覆盖', async () => {
  const e = env();
  const first = await upload(e, envelope());
  assert.equal(first.body.replaced, false);
  const again = await upload(e, envelope({ rows: 999 }));
  assert.equal(again.res.status, 200);
  assert.equal(again.body.replaced, true, '命中已有对象时 replaced 必须是 true');
  assert.equal(e.AUDIT_BUCKET.map.size, 1, '重传只覆盖，不产生第二个对象');
  const stored = JSON.parse(new TextDecoder().decode(e.AUDIT_BUCKET.map.get(again.body.key).value));
  assert.equal(stored.rows, 999, '内容应被最新一次覆盖');
  // 另一天 ⇒ 新 key，replaced 又是 false
  const other = await upload(e, envelope({ day: '2026-09-25' }));
  assert.equal(other.body.replaced, false);
  assert.equal(e.AUDIT_BUCKET.map.size, 2);
});

test('响应：成功体里**不出现** ct / nonce 的内容', async () => {
  const e = env();
  const { text, body } = await upload(e, envelope());
  assert.ok(!text.includes(CT), '成功响应里不许回显 ct');
  assert.ok(!text.includes(CT.slice(0, 16)), '成功响应里不许回显 ct 片段');
  assert.ok(!text.includes(NONCE), '成功响应里不许回显 nonce');
  assert.deepEqual(Object.keys(body).sort(), ['key', 'ok', 'replaced']);
});

// ------------------------------------------------------------------ list

test('list：返回元数据（day/rows/bytes/key/uploaded_unix）并按 day 升序', async () => {
  const e = env();
  const days = ['2026-09-03', '2026-09-01', '2026-09-02'];
  for (const day of days) {
    await upload(e, envelope({ day, rows: Number(day.slice(-2)), bytes: 100 + Number(day.slice(-2)) }));
  }
  const { res, body } = await call('/list', e, { obj: { device: DEV } });
  assert.equal(res.status, 200);
  assert.equal(body.ok, true);
  assert.equal(body.items.length, 3);
  assert.deepEqual(body.items.map((x) => x.day), ['2026-09-01', '2026-09-02', '2026-09-03']);
  for (const it of body.items) {
    assert.equal(it.key, `audit/${DEV}/${it.day}.json`);
    assert.ok(Number.isInteger(it.rows) && it.rows >= 0);
    assert.ok(Number.isInteger(it.bytes) && it.bytes >= 0);
    assert.ok(Number.isInteger(it.uploaded_unix), 'uploaded_unix 应来自 R2 对象 uploaded');
    assert.ok(Math.abs(it.uploaded_unix - Math.floor(Date.now() / 1000)) < 60);
  }
  const byDay = Object.fromEntries(body.items.map((x) => [x.day, x]));
  assert.equal(byDay['2026-09-01'].rows, 1);
  assert.equal(byDay['2026-09-02'].bytes, 102);
});

test('list：**必须分页** —— R2 一次只回 2 条时仍要列全（并真的多列了几次）', async () => {
  const bucket = fakeR2({ pageLimit: 2 });
  const e = env({ AUDIT_BUCKET: bucket });
  for (const day of ['2026-09-01', '2026-09-02', '2026-09-03', '2026-09-04', '2026-09-05']) {
    await upload(e, envelope({ day, rows: 1 }));
  }
  const before = bucket.listCalls;
  const { res, body } = await call('/list', e, { obj: { device: DEV } });
  assert.equal(res.status, 200);
  assert.equal(body.items.length, 5, '分页没做全 ⇒ 这里会少');
  assert.ok(bucket.listCalls - before >= 3, `应发生多次 list（实际 ${bucket.listCalls - before} 次）`);
});

test('list：只返回本 device 的对象；空设备 ⇒ 空数组', async () => {
  const e = env();
  await upload(e, envelope({ device: DEV, day: '2026-09-01' }));
  await upload(e, envelope({ device: 'aaaaaaaaaaaaaaaa', day: '2026-09-02' }));
  const mine = await call('/list', e, { obj: { device: DEV } });
  assert.equal(mine.body.items.length, 1);
  assert.equal(mine.body.items[0].key, `audit/${DEV}/2026-09-01.json`);
  const none = await call('/list', e, { obj: { device: 'ffffffffffffffff' } });
  assert.deepEqual(none.body.items, []);
});

test('list：device 形状不对 ⇒ 400（不能拿未校验的 device 当前缀）', async () => {
  const e = env();
  for (const device of ['../x', '', 'a'.repeat(17), 'ABCDEF0123456789']) {
    const { res, body } = await call('/list', e, { obj: { device } });
    assert.equal(res.status, 400, `device=${JSON.stringify(device)}`);
    assert.equal(body.error, 'bad_device');
  }
  const badBody = await call('/list', e, { raw: '{}' });
  assert.equal(badBody.res.status, 400);
  assert.equal(badBody.body.error, 'bad_device');
});

test('list：R2 没给 uploaded ⇒ uploaded_unix = null（**不编造**）', async () => {
  const e = env();
  seed(e.AUDIT_BUCKET, DEV, '2026-09-09', envelope({ day: '2026-09-09' }), undefined);
  const { body } = await call('/list', e, { obj: { device: DEV } });
  assert.equal(body.items.length, 1);
  assert.equal(body.items[0].uploaded_unix, null);
});

test('list：对象读不出/坏 JSON ⇒ rows/bytes = null（不编造），其余字段仍给', async () => {
  const e = env();
  e.AUDIT_BUCKET.map.set(`audit/${DEV}/2026-09-09.json`, {
    value: new TextEncoder().encode('{broken'),
    opts: {},
    uploaded: new Date(),
  });
  const { body } = await call('/list', e, { obj: { device: DEV } });
  assert.equal(body.items.length, 1);
  assert.equal(body.items[0].rows, null);
  assert.equal(body.items[0].bytes, null);
  assert.equal(body.items[0].day, '2026-09-09');
});

test('list：惰性过期兜底 —— 超过 RETENTION_DAYS 的对象被剔除并删除', async () => {
  const e = env({ RETENTION_DAYS: '400' });
  const old = new Date(Date.now() - 401 * 86400 * 1000);
  seed(e.AUDIT_BUCKET, DEV, '2025-01-01', envelope({ day: '2025-01-01' }), old);
  seed(e.AUDIT_BUCKET, DEV, '2026-09-09', envelope({ day: '2026-09-09' }), new Date());
  const { body } = await call('/list', e, { obj: { device: DEV } });
  assert.deepEqual(body.items.map((x) => x.day), ['2026-09-09']);
  assert.ok(!e.AUDIT_BUCKET.map.has(`audit/${DEV}/2025-01-01.json`), '过期对象应被顺手删掉');
});

// ------------------------------------------------------------------ revoke

test('revoke：删掉本 device 的**全部**对象，**只删本 device**，并分页删完', async () => {
  const bucket = fakeR2({ pageLimit: 1 }); // 一次只列 1 条 ⇒ 必须真分页
  const e = env({ AUDIT_BUCKET: bucket });
  const other = 'aaaaaaaaaaaaaaaa';
  for (const day of ['2026-09-01', '2026-09-02', '2026-09-03']) await upload(e, envelope({ day }));
  for (const day of ['2026-09-04', '2026-09-05']) await upload(e, envelope({ device: other, day }));

  const before = bucket.listCalls;
  const { res, body } = await call('/revoke', e, { obj: { device: DEV } });
  assert.equal(res.status, 200);
  assert.equal(body.ok, true);
  assert.equal(body.deleted, 3, '分页没删完 ⇒ 这里小于 3（只删第一页就是假成功）');
  assert.ok(bucket.listCalls - before >= 3, 'revoke 也必须分页 list 到底');

  // 另一个 device 一个字都没少
  for (const day of ['2026-09-04', '2026-09-05']) {
    assert.ok(e.AUDIT_BUCKET.map.has(`audit/${other}/${day}.json`), `不该删 ${other}/${day}`);
  }
  assert.equal(e.AUDIT_BUCKET.map.size, 2);
  const rest = await call('/list', e, { obj: { device: other } });
  assert.equal(rest.body.items.length, 2);
});

test('revoke：本 device 为空 ⇒ deleted=0（不是错误）', async () => {
  const e = env();
  const { res, body } = await call('/revoke', e, { obj: { device: 'ffffffffffffffff' } });
  assert.equal(res.status, 200);
  assert.equal(body.deleted, 0);
});

test('revoke：device 形状不对 ⇒ 400，且**不误删任何对象**', async () => {
  const e = env();
  await upload(e, envelope());
  for (const device of ['../x', '', 'ABCDEF0123456789']) {
    const { res } = await call('/revoke', e, { obj: { device } });
    assert.equal(res.status, 400, `device=${JSON.stringify(device)}`);
  }
  assert.equal(e.AUDIT_BUCKET.map.size, 1, '非法 device 的 revoke 不能碰数据');
});

test('revoke 之后 list 为空（撤回是端到端可见的）', async () => {
  const e = env();
  await upload(e, envelope({ day: '2026-09-01' }));
  await upload(e, envelope({ day: '2026-09-02' }));
  await call('/revoke', e, { obj: { device: DEV } });
  const { body } = await call('/list', e, { obj: { device: DEV } });
  assert.deepEqual(body.items, []);
});

// ------------------------------------------------------------------ 路由 / 404 / 405

test('路由：路径不对 ⇒ 404；端点外的路径 ⇒ 404', async () => {
  const e = env();
  assert.equal((await call('/nope', e, { obj: envelope() })).res.status, 404);
  assert.equal((await call('/list/extra', e, { obj: { device: DEV } })).res.status, 404);
  assert.equal(
    (await worker.handle(new Request('https://xraytun.top/other', { method: 'POST' }), e)).status,
    404,
  );
  assert.equal(
    (await worker.handle(new Request('https://xraytun.top/api/auditx', { method: 'POST' }), e)).status,
    404,
  );
});

test('路由：错误响应体格式与 incident-collector 一致（error + message）', async () => {
  const e = env();
  const { res, body } = await call('/nope', e, { obj: {} });
  assert.equal(res.status, 404);
  assert.equal(typeof body.error, 'string');
  assert.equal(typeof body.message, 'string');
  assert.equal(res.headers.get('cache-control'), 'no-store');
  assert.ok(res.headers.get('content-type').startsWith('application/json'));
});

test('路由：路由对但方法不对 ⇒ 405（三个路径都是 POST-only）', async () => {
  const e = env();
  const get = await worker.handle(req('', { method: 'GET', obj: undefined }), e);
  assert.equal(get.status, 405);
  const del = await worker.handle(req('/list', { method: 'DELETE', obj: { device: DEV } }), e);
  assert.equal(del.status, 405);
  const put = await worker.handle(req('/revoke', { method: 'PUT', obj: { device: DEV } }), e);
  assert.equal(put.status, 405);
});

test('路由：基路径尾斜杠可接受（`/api/audit/` = `/api/audit`）', async () => {
  const e = env();
  const res = await worker.handle(new Request(`${BASE}/`, {
    method: 'POST',
    headers: { authorization: `Bearer ${TOKEN}`, 'content-type': 'application/json', 'CF-Connecting-IP': IP },
    body: JSON.stringify(envelope()),
  }), e);
  assert.equal(res.status, 200);
  assert.equal(e.AUDIT_BUCKET.map.size, 1);
});

test('内部错误不泄漏细节：坏 R2 替身 ⇒ 500 + 固定文案', async () => {
  const e = env({
    AUDIT_BUCKET: {
      async head() {
        throw new Error('boom: secret-internal-path');
      },
      async put() {},
    },
  });
  // 走 default export 的那层 try/catch
  const res = await worker.default.fetch(req('', { obj: envelope() }), e, {});
  assert.equal(res.status, 500);
  const text = await res.text();
  assert.ok(!text.includes('boom'), '内部错误细节不许回给调用方');
  assert.ok(!text.includes('secret-internal-path'));
});
