/**
 * 现场包端点的本地自测（**不需要 Cloudflare 账号**）：`node --test test/worker.test.mjs`
 *
 * 覆盖卡面点名的每个分支：大小上限 / 类型白名单（含 zip 魔数）/ 限流 / 鉴权 / 删除 /
 * manifest 不含 IP / 惰性过期 / 路由与方法边界。
 *
 * `INCIDENT_MODULE` 环境变量可指向**另一份** worker 源码 —— `verify.sh --sensitivity`
 * 就是用它把「鉴权被拿掉」「大小上限被拿掉」的变体喂进来，证明这些用例**真的在验**那两条。
 */
import test from 'node:test';
import assert from 'node:assert/strict';

const MOD = process.env.INCIDENT_MODULE || new URL('../src/worker.mjs', import.meta.url).href;
const worker = await import(MOD);

// 每个用例都从干净的限流状态开始：限流是**模块级内存**，不 reset 会串味
// （第一版就踩了这个：默认 5 次/小时，后面的用例拿到 429 而不是各自要验的状态码）
test.beforeEach(() => worker.resetRateLimits());

const BASE = 'https://xraytun.top/api/incident';
const TOKEN = 'test-token-do-not-use';
const IP = '203.0.113.7';

// ------------------------------------------------------------------ 测试替身

function fakeR2() {
  const map = new Map();
  return {
    map,
    async put(key, value, opts) {
      map.set(key, { value, opts });
    },
    async get(key) {
      const e = map.get(key);
      if (!e) return null;
      const bytes = e.value instanceof Uint8Array ? e.value : new TextEncoder().encode(String(e.value));
      return {
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
  };
}

function env(extra = {}) {
  return {
    INCIDENT_BUCKET: fakeR2(),
    INCIDENT_TOKEN: TOKEN,
    ...extra,
  };
}

// ------------------------------------------------------------------ 真 zip 构造器
//
// 端点现在会**解析 zip** 做隐私扫描（fail closed）⇒ 测试必须喂**真的 zip**，
// 不能再用「魔数 + 填充」。这里用 store（不压缩）与 deflate-raw 两种方式各造几份。

const CRC_TABLE = (() => {
  const t = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c >>> 0;
  }
  return t;
})();

function crc32(bytes) {
  let c = 0xffffffff;
  for (let i = 0; i < bytes.length; i++) c = CRC_TABLE[(c ^ bytes[i]) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

async function deflateRaw(bytes) {
  const cs = new CompressionStream('deflate-raw');
  const stream = new Response(bytes).body.pipeThrough(cs);
  return new Uint8Array(await new Response(stream).arrayBuffer());
}

/** 构造一个合法 zip。entries: [{name, text}]；method: 0=store, 8=deflate-raw。 */
async function makeZip(entries, method = 0) {
  const enc = new TextEncoder();
  const chunks = [];
  const central = [];
  let offset = 0;
  for (const e of entries) {
    const name = enc.encode(e.name);
    const raw = enc.encode(e.text);
    const data = method === 8 ? await deflateRaw(raw) : raw;
    const crc = crc32(raw);
    const lh = new Uint8Array(30 + name.length);
    const dv = new DataView(lh.buffer);
    dv.setUint32(0, 0x04034b50, true);
    dv.setUint16(4, 20, true); dv.setUint16(6, 0, true); dv.setUint16(8, method, true);
    dv.setUint16(10, 0, true); dv.setUint16(12, 0x21, true); // 固定时间戳：结果可复现
    dv.setUint32(14, crc, true); dv.setUint32(18, data.length, true); dv.setUint32(22, raw.length, true);
    dv.setUint16(26, name.length, true); dv.setUint16(28, 0, true);
    lh.set(name, 30);
    chunks.push(lh, data);
    const cd = new Uint8Array(46 + name.length);
    const cv = new DataView(cd.buffer);
    cv.setUint32(0, 0x02014b50, true);
    cv.setUint16(4, 20, true); cv.setUint16(6, 20, true); cv.setUint16(8, 0, true);
    cv.setUint16(10, method, true); cv.setUint16(12, 0, true); cv.setUint16(14, 0x21, true);
    cv.setUint32(16, crc, true); cv.setUint32(20, data.length, true); cv.setUint32(24, raw.length, true);
    cv.setUint16(28, name.length, true);
    cv.setUint32(42, offset, true);
    cd.set(name, 46);
    central.push(cd);
    offset += lh.length + data.length;
  }
  const cdSize = central.reduce((n, c) => n + c.length, 0);
  const eocd = new Uint8Array(22);
  const ev = new DataView(eocd.buffer);
  ev.setUint32(0, 0x06054b50, true);
  ev.setUint16(8, central.length, true); ev.setUint16(10, central.length, true);
  ev.setUint32(12, cdSize, true); ev.setUint32(16, offset, true);
  const all = [...chunks, ...central, eocd];
  const total = all.reduce((n, c) => n + c.length, 0);
  const out = new Uint8Array(total);
  let at = 0;
  for (const c of all) { out.set(c, at); at += c.length; }
  return out;
}

/** 默认包：一个干净的 zip（内容固定 ⇒ 字节可复现）。 */
async function cleanZip(n = 64) {
  return makeZip([{ name: 'README.txt', text: `XrayTun incident bundle (synthetic test fixture)\n${'x'.repeat(n)}` }]);
}

function uploadRequest({ bytes, ctype = 'application/zip', ip = IP, headers = {} } = {}) {
  return new Request(BASE, {
    method: 'POST',
    headers: { 'content-type': ctype, 'CF-Connecting-IP': ip, ...headers },
    body: bytes,
  });
}

async function doUpload(e, opts = {}) {
  const bytes = opts.bytes || (await cleanZip());
  const res = await worker.handle(uploadRequest({ ...opts, bytes }), e);
  const body = res.status === 201 ? await res.json() : await res.json().catch(() => null);
  return { res, body };
}

// ------------------------------------------------------------------ 上传

test('上传：合法 zip ⇒ 201，返回 id/sha256/received_at/bytes', async () => {
  const e = env();
  const { res, body } = await doUpload(e);
  assert.equal(res.status, 201);
  assert.match(body.id, worker.ID_RE);
  assert.match(body.sha256, /^[0-9a-f]{64}$/);
  assert.ok(body.received_at.endsWith('Z'));
  assert.equal(body.bytes, (await cleanZip()).length);
  // 真的落盘了：manifest + zip 两个键
  assert.ok(e.INCIDENT_BUCKET.map.has(`${body.id}/manifest.json`));
  assert.ok(e.INCIDENT_BUCKET.map.has(`${body.id}.zip`));
});

test('上传：超出大小上限 ⇒ 413（size 上限被拿掉时本用例会红）', async () => {
  const e = env({ MAX_BYTES: '1024' });
  const { res, body } = await doUpload(e, { bytes: await cleanZip(4096) });
  assert.equal(res.status, 413);
  assert.equal(body.error, 'too_large');
  assert.equal(body.max_bytes, 1024);
  assert.equal(e.INCIDENT_BUCKET.map.size, 0, '被拒绝的上传不该落盘');
});

test('上传：content-type 不在白名单 ⇒ 415', async () => {
  const e = env();
  const { res, body } = await doUpload(e, { ctype: 'text/plain' });
  assert.equal(res.status, 415);
  assert.equal(body.error, 'bad_type');
});

test('上传：content-type 说 zip 但魔数不对 ⇒ 415（不认自述）', async () => {
  const e = env();
  const notZip = new Uint8Array(64).fill(0x41);
  const { res, body } = await doUpload(e, { bytes: notZip });
  assert.equal(res.status, 415);
  assert.equal(body.error, 'not_zip');
});

test('上传：限流（第 3 次 ⇒ 429，带 Retry-After）', async () => {
  const e = env({ RATE_LIMIT_MAX: '2', RATE_LIMIT_WINDOW_SECONDS: '3600' });
  await doUpload(e, { ip: '198.51.100.9' });
  await doUpload(e, { ip: '198.51.100.9' });
  const { res, body } = await doUpload(e, { ip: '198.51.100.9' });
  assert.equal(res.status, 429);
  assert.equal(body.error, 'rate_limited');
  assert.ok(body.retry_after_seconds >= 1);
  assert.equal(res.headers.get('retry-after'), String(body.retry_after_seconds));
  // 换一个 IP 不受影响（限流是「按 IP」而不是全局）
  const other = await doUpload(e, { ip: '198.51.100.10' });
  assert.equal(other.res.status, 201);
});

test('上传：manifest **不含 IP**（即便请求头里有）', async () => {
  const e = env();
  const { body } = await doUpload(e, { ip: '203.0.113.99' });
  const raw = e.INCIDENT_BUCKET.map.get(`${body.id}/manifest.json`).value;
  assert.ok(!raw.includes('203.0.113.99'), 'manifest 里不许出现客户端 IP');
  const manifest = JSON.parse(raw);
  assert.ok(!('ip' in manifest));
  assert.ok(!('client_ip' in manifest));
  assert.ok(!('user_agent' in manifest));
});

test('上传：客户端自述版本只在不违反格式时才保留', async () => {
  const e = env();
  const ok = await doUpload(e, { headers: { 'X-XrayTun-Client': 'XrayTun/0.8.34' } });
  assert.equal(JSON.parse(e.INCIDENT_BUCKET.map.get(`${ok.body.id}/manifest.json`).value).client_self_declared, 'XrayTun/0.8.34');
  // 注意：带 CRLF 的 header 值连 `new Request()` 都构造不出来（undici 与 CF 都会先拒绝），
  // 所以这里用「合法 header 值、但不匹配白名单字符集」的样本。
  const bad = await doUpload(e, { ip: '198.51.100.20', headers: { 'X-XrayTun-Client': 'evil value! ; rm -rf /' } });
  assert.ok(!('client_self_declared' in JSON.parse(e.INCIDENT_BUCKET.map.get(`${bad.body.id}/manifest.json`).value)));
});

// ------------------------------------------------------------------ manifest

test('manifest：公开可读，且形如约定（无日志正文）', async () => {
  const e = env();
  const { body } = await doUpload(e);
  const res = await worker.handle(new Request(`${BASE}/${body.id}`, { headers: { 'CF-Connecting-IP': IP } }), e);
  assert.equal(res.status, 200);
  const m = await res.json();
  assert.equal(m.id, body.id);
  assert.equal(m.sha256, body.sha256);
  assert.equal(m.schema_version, 1);
  assert.equal(m.retention_days, 30);
});

test('manifest：id 格式不对 ⇒ 400；不存在 ⇒ 404', async () => {
  const e = env();
  assert.equal((await worker.handle(new Request(`${BASE}/not-an-id`), e)).status, 400);
  assert.equal((await worker.handle(new Request(`${BASE}/INC-20260101-000000-abcd`), e)).status, 404);
});

// ------------------------------------------------------------------ blob 鉴权

test('blob：不带 token ⇒ 401；错 token ⇒ 401；对 token ⇒ 200 + 原始字节', async () => {
  const e = env();
  const { body } = await doUpload(e);
  const url = `${BASE}/${body.id}/blob`;
  assert.equal((await worker.handle(new Request(url), e)).status, 401);
  assert.equal((await worker.handle(new Request(url, { headers: { 'X-Auth-Token': 'wrong' } }), e)).status, 401);
  const ok = await worker.handle(new Request(url, { headers: { 'X-Auth-Token': TOKEN } }), e);
  assert.equal(ok.status, 200);
  assert.equal(ok.headers.get('content-type'), 'application/zip');
  assert.equal(ok.headers.get('x-incident-sha256'), body.sha256);
  const got = new Uint8Array(await ok.arrayBuffer());
  assert.deepEqual(got, await cleanZip());
});

test('blob：没配置 INCIDENT_TOKEN 时 ⇒ 一律 401（fail closed）', async () => {
  const e = env({ INCIDENT_TOKEN: '' });
  const { body } = await doUpload(e);
  const res = await worker.handle(new Request(`${BASE}/${body.id}/blob`, { headers: { 'X-Auth-Token': '' } }), e);
  assert.equal(res.status, 401);
});

// ------------------------------------------------------------------ 删除

test('删除：不带 token ⇒ 401 且什么都没删；带 token ⇒ 200，随后 manifest 404', async () => {
  const e = env();
  const { body } = await doUpload(e);
  const url = `${BASE}/${body.id}`;
  assert.equal((await worker.handle(new Request(url, { method: 'DELETE' }), e)).status, 401);
  assert.ok(e.INCIDENT_BUCKET.map.has(`${body.id}.zip`), '未授权删除不该动数据');
  const del = await worker.handle(new Request(url, { method: 'DELETE', headers: { 'X-Auth-Token': TOKEN } }), e);
  assert.equal(del.status, 200);
  assert.equal((await del.json()).deleted, true);
  assert.equal(e.INCIDENT_BUCKET.map.size, 0);
  assert.equal((await worker.handle(new Request(url), e)).status, 404);
});

test('删除：不存在的 id ⇒ 404（带 token）', async () => {
  const e = env();
  const res = await worker.handle(
    new Request(`${BASE}/INC-20260101-000000-abcd`, { method: 'DELETE', headers: { 'X-Auth-Token': TOKEN } }),
    e,
  );
  assert.equal(res.status, 404);
});

// ------------------------------------------------------------------ 保留期

test('保留期：超过 30 天的 manifest ⇒ 404 expired，且顺手清掉两个键', async () => {
  const e = env();
  const { body } = await doUpload(e);
  const key = `${body.id}/manifest.json`;
  const m = JSON.parse(e.INCIDENT_BUCKET.map.get(key).value);
  m.received_at = new Date(Date.now() - 31 * 86400 * 1000).toISOString();
  e.INCIDENT_BUCKET.map.get(key).value = JSON.stringify(m);
  const res = await worker.handle(new Request(`${BASE}/${body.id}`), e);
  assert.equal(res.status, 404);
  assert.equal((await res.json()).error, 'expired');
  assert.equal(e.INCIDENT_BUCKET.map.size, 0, '过期项应被删除');
});

// ------------------------------------------------------------------ 路由边界

test('路由：POST 子路径 ⇒ 404；GET 基路径 ⇒ 404；PUT ⇒ 405；尾斜杠 POST 可用', async () => {
  const e = env();
  assert.equal((await worker.handle(new Request(`${BASE}/x`, { method: 'POST', headers: { 'content-type': 'application/zip' }, body: await cleanZip() }), e)).status, 404);
  assert.equal((await worker.handle(new Request(BASE), e)).status, 404);
  assert.equal((await worker.handle(new Request(BASE, { method: 'PUT' }), e)).status, 405);
  const trailing = await worker.handle(uploadRequest({ bytes: await cleanZip() }), e);
  assert.equal(trailing.status, 201);
  const withSlash = await worker.handle(new Request(`${BASE}/`, { method: 'POST', headers: { 'content-type': 'application/zip', 'CF-Connecting-IP': '198.51.100.77' }, body: await cleanZip() }), e);
  assert.equal(withSlash.status, 201);
});

test('路由：不在本端点下的路径 ⇒ 404', async () => {
  const e = env();
  assert.equal((await worker.handle(new Request('https://xraytun.top/other'), e)).status, 404);
  assert.equal((await worker.handle(new Request('https://xraytun.top/api/incidentx'), e)).status, 404);
});

// ------------------------------------------------------------------ 隐私拒收（最后防线）

test('隐私：包内含 vless 订阅 URL ⇒ 422，命中类型=node_url，且**不回显密钥原文**', async () => {
  const e = env();
  const secret = 'vless://11111111-2222-3333-4444-555555555555@example.com:443?security=reality#node';
  const zip = await makeZip([{ name: 'nodes.txt', text: `节点\n${secret}\n` }]);
  const { res, body } = await doUpload(e, { bytes: zip });
  assert.equal(res.status, 422);
  assert.equal(body.error, 'secret_detected');
  assert.ok(body.hits.some((h) => h.type === 'node_url'), JSON.stringify(body.hits));
  assert.equal(e.INCIDENT_BUCKET.map.size, 0, '被拒收的包绝不能落盘');
  const raw = JSON.stringify(body);
  assert.ok(!raw.includes(secret), '响应体里不许出现密钥原文');
  assert.ok(!raw.includes('11111111-2222-3333-4444-555555555555'), '响应体里不许出现 UUID 原文');
  assert.ok(body.hits.every((h) => h.file && h.line > 0), '命中要给出文件与行号');
});

test('隐私：JSON 形键值（客户端漏过的那种）⇒ 422', async () => {
  const e = env();
  const zip = await makeZip([{ name: 'runtime/config.json', text: '{"password":"hunter2secret","port":10808}' }]);
  const { res, body } = await doUpload(e, { bytes: zip });
  assert.equal(res.status, 422);
  assert.ok(body.hits.some((h) => h.type === 'json_secret_key'), JSON.stringify(body.hits));
  assert.ok(!JSON.stringify(body).includes('hunter2secret'));
});

test('隐私：URI 的 ?query/#fragment 里的 pbk=/sid= ⇒ 422', async () => {
  const e = env();
  const zip = await makeZip([{ name: 'sub.url', text: 'https://example.com/sub?pbk=SECRETPBKVALUE&sid=9f8e7d6c#frag' }]);
  const { res, body } = await doUpload(e, { bytes: zip });
  assert.equal(res.status, 422);
  assert.ok(body.hits.some((h) => h.type === 'uri_secret_param'), JSON.stringify(body.hits));
});

test('隐私：PEM 私钥块 ⇒ 422；UUID 字面量 ⇒ 422；但 <uuid> 占位符 ⇒ 201', async () => {
  const e = env();
  const pem = await doUpload(e, { bytes: await makeZip([{ name: 'key.pem', text: '-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n' }]), ip: '198.51.100.31' });
  assert.equal(pem.res.status, 422);
  assert.ok(pem.body.hits.some((h) => h.type === 'private_key_pem'));

  const uuid = await doUpload(e, { bytes: await makeZip([{ name: 'cfg.json', text: '{"node_uuid":"7c9e6679-7425-40de-944b-e07fc1f90ae7"}' }]), ip: '198.51.100.32' });
  assert.equal(uuid.res.status, 422);
  assert.ok(uuid.body.hits.some((h) => h.type === 'uuid_literal' || h.type === 'json_secret_key'), JSON.stringify(uuid.body.hits));

  const placeholder = await doUpload(e, { bytes: await makeZip([{ name: 'README.txt', text: '把你的 <uuid> 填到这里（示例：00000000-0000-0000-0000-000000000000）' }]), ip: '198.51.100.33' });
  assert.equal(placeholder.res.status, 201, '占位符不该被当成密钥');
});

test('隐私：deflate 压缩的条目同样要扫到（不能只扫 store）', async () => {
  const e = env();
  const zip = await makeZip([{ name: 'logs/core.log', text: 'x\n'.repeat(50) + 'vless://aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee@h:443\n' }], 8);
  const { res, body } = await doUpload(e, { bytes: zip });
  assert.equal(res.status, 422);
  assert.ok(body.hits.some((h) => h.type === 'node_url'), JSON.stringify(body.hits));
});

test('隐私：命中列表**限长**（最多 20 条，且标 truncated）', async () => {
  const e = env();
  const many = Array.from({ length: 40 }, (_, i) => `vless://node${i}@example.com:443`).join('\n');
  const { res, body } = await doUpload(e, { bytes: await makeZip([{ name: 'nodes.txt', text: many }]) });
  assert.equal(res.status, 422);
  assert.ok(body.hits.length <= 20, `hits 应被截断，实际 ${body.hits.length}`);
  assert.equal(body.hits_truncated, true);
});

test('隐私：**fail closed** —— 魔数对但结构坏的 zip ⇒ 422 scan_failed（不是放行）', async () => {
  const e = env();
  const broken = new Uint8Array(64);
  broken[0] = 0x50; broken[1] = 0x4b; broken[2] = 0x03; broken[3] = 0x04; // 只有魔数像 zip
  const { res, body } = await doUpload(e, { bytes: broken });
  assert.equal(res.status, 422);
  assert.equal(body.error, 'scan_failed');
  assert.equal(e.INCIDENT_BUCKET.map.size, 0);
});
