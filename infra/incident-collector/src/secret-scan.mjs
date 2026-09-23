/**
 * 服务端「疑似密钥」扫描（最后防线）—— task-114 追加要求
 *
 * 为什么必须在服务端也扫一遍：客户端（`task-113` 的打包脚本）负责脱敏，但它**当场被 tester
 * 抓到自己三处漏**（JSON 形键值、URI 的 `?query/#fragment`、`Authorization` 只吃掉 `Bearer`）。
 * 而端点会把包**原样存 30 天** ⇒ 客户端漏一次，密钥就在云上躺一个月。
 * 两层互不替代：客户端是正常路径，这里是**最后防线**。
 *
 * 设计口径：
 *   · **fail closed**：包解析不了 / 解压失败 / 任何异常 ⇒ 判为「无法确认安全」⇒ 拒收（不是放行）；
 *   · 命中列表**限长**（`MAX_HITS`），且**只回报「类型 + 文件 + 行号」**，**绝不回显密钥原文**
 *     （否则响应体本身就成了泄漏面）；
 *   · 只做**浅层**文本扫描（zip 内文本条目），不追求穷尽 —— 它挡的是「明显没脱敏」，不是保险箱。
 */

export const MAX_HITS = 20; // 命中列表上限（限长，避免响应变成泄漏面）
export const MAX_SCAN_BYTES = 4 * 1024 * 1024; // 最多解压扫描这么多字节（CPU 上限）
export const MAX_FILES = 400; // 最多扫这么多条目

/** 允许「明显是占位符」的 UUID：`<uuid>` / `{{uuid}}` / 全 x / 全 0 / YOUR_UUID 这类。 */
const UUID_RE = /\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b/gi;
const PLACEHOLDER_RE = /<uuid>|\{\{?\s*uuid\s*\}?\}|your[_-]?uuid|xxxxxxxx-|00000000-0000-0000-0000-000000000000/i;

const PATTERNS = [
  {
    type: 'private_key_pem',
    // 只报「有 PEM 私钥块」，不回报块内容
    re: /-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----/g,
  },
  {
    type: 'node_url',
    re: /\b(?:vless|vmess|ss|ssr|trojan|hysteria2?|tuic):\/\/[^\s"'<>]{8,}/gi,
  },
  {
    type: 'uri_secret_param',
    // ?pbk= / #sid= / &token= … 值至少 8 个字符（避免 `key=` 这类正常词误报）
    re: /[?&#](?:pbk|sid|spx|token|password|passwd|pwd|uuid|key|secret|apikey|api_key)=[A-Za-z0-9+/=_.~%-]{8,}/gi,
  },
  {
    type: 'json_secret_key',
    // "password": "…" / "node_uuid":"…" —— 值至少 8 字符
    re: /"(?:password|passwd|pwd|token|secret|api_?key|uuid|node_?uuid|pbk|sid|spx)"\s*:\s*"[^"]{8,}"/gi,
  },
  {
    type: 'uuid_literal',
    re: UUID_RE,
    // UUID 单独判：占位符不算（`<uuid>`、全 x、全 0、your-uuid …）
    filter: (m, text, at) => {
      const around = text.slice(Math.max(0, at - 12), at + 48);
      if (PLACEHOLDER_RE.test(around)) return false;
      const body = m.toLowerCase();
      if (/^([0-9a-f])\1{7}-/.test(body)) return false; // 同一个字符重复（xxxx…）
      if (/^0{8}-0{4}-0{4}-0{4}-0{12}$/.test(body)) return false;
      return true;
    },
  },
];

function lineOf(text, index) {
  let line = 1;
  for (let i = 0; i < index && i < text.length; i++) if (text.charCodeAt(i) === 10) line++;
  return line;
}

/** 扫一段文本；hits 达到上限就停止（并标记 truncated）。 */
export function scanText(name, text, hits) {
  let truncated = false;
  for (const p of PATTERNS) {
    if (hits.length >= MAX_HITS) {
      truncated = true;
      break;
    }
    p.re.lastIndex = 0;
    let m;
    while ((m = p.re.exec(text)) !== null) {
      if (p.filter && !p.filter(m[0], text, m.index)) continue;
      hits.push({ type: p.type, file: name, line: lineOf(text, m.index) });
      if (hits.length >= MAX_HITS) {
        truncated = true;
        break;
      }
      if (m.index === p.re.lastIndex) p.re.lastIndex++; // 防零宽匹配死循环
    }
  }
  return truncated;
}

// ------------------------------------------------------------------ 极简 zip 读取

function u16(b, o) {
  return b[o] | (b[o + 1] << 8);
}
function u32(b, o) {
  return (b[o] | (b[o + 1] << 8) | (b[o + 2] << 16)) + (b[o + 3] << 24 >>> 0);
}

/** 找 EOCD（`PK\x05\x06`），从尾部往前扫。 */
function findEocd(b) {
  for (let i = b.length - 22; i >= 0 && i >= b.length - 22 - 65536; i--) {
    if (b[i] === 0x50 && b[i + 1] === 0x4b && b[i + 2] === 0x05 && b[i + 3] === 0x06) return i;
  }
  return -1;
}

function readEntries(bytes) {
  const b = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
  const eocd = findEocd(b);
  if (eocd < 0) throw new Error('找不到 zip 中央目录（EOCD）');
  const count = u16(b, eocd + 10);
  let off = u32(b, eocd + 16);
  const out = [];
  for (let i = 0; i < count && i < MAX_FILES; i++) {
    if (!(b[off] === 0x50 && b[off + 1] === 0x4b && b[off + 2] === 0x01 && b[off + 3] === 0x02)) {
      throw new Error('中央目录条目签名不对');
    }
    const method = u16(b, off + 10);
    const compSize = u32(b, off + 20);
    const nameLen = u16(b, off + 28);
    const extraLen = u16(b, off + 30);
    const commentLen = u16(b, off + 32);
    const localOff = u32(b, off + 42);
    const name = new TextDecoder('utf-8', { fatal: false }).decode(b.subarray(off + 46, off + 46 + nameLen));
    out.push({ name, method, compSize, localOff });
    off += 46 + nameLen + extraLen + commentLen;
  }
  return out;
}

async function readEntryData(bytes, entry) {
  const b = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
  const lo = entry.localOff;
  if (!(b[lo] === 0x50 && b[lo + 1] === 0x4b && b[lo + 2] === 0x03 && b[lo + 3] === 0x04)) {
    throw new Error('本地文件头签名不对');
  }
  const nameLen = u16(b, lo + 26);
  const extraLen = u16(b, lo + 28);
  const start = lo + 30 + nameLen + extraLen;
  const raw = b.subarray(start, start + entry.compSize);
  if (entry.method === 0) return raw;
  if (entry.method !== 8) throw new Error(`不支持的压缩方式：${entry.method}`);
  const ds = new DecompressionStream('deflate-raw');
  const stream = new Response(raw).body.pipeThrough(ds);
  const buf = await new Response(stream).arrayBuffer();
  return new Uint8Array(buf);
}

// ------------------------------------------------------------------ 对外入口

/**
 * 扫 zip 里的文本，找「疑似密钥」。
 * @returns {Promise<{ok:boolean, hits:Array, truncated:boolean, reason?:string, scanned_files:number, scanned_bytes:number}>}
 *   `ok:false` = 拒收（命中 **或** 扫描失败 —— fail closed）。
 */
export async function scanZipForSecrets(bytes, opts = {}) {
  const maxHits = opts.maxHits || MAX_HITS;
  const maxBytes = opts.maxBytes || MAX_SCAN_BYTES;
  const hits = [];
  let truncated = false;
  let scannedFiles = 0;
  let scannedBytes = 0;
  try {
    const entries = readEntries(bytes);
    for (const e of entries) {
      if (scannedBytes >= maxBytes) {
        truncated = true;
        break;
      }
      if (e.name.endsWith('/')) continue; // 目录项
      let data;
      try {
        data = await readEntryData(bytes, e);
      } catch (err) {
        // fail closed：解不开的条目不能当成「没问题」
        return { ok: false, hits, truncated, reason: `unreadable_entry:${e.name}`, scanned_files: scannedFiles, scanned_bytes: scannedBytes };
      }
      if (data.byteLength > maxBytes - scannedBytes) data = data.subarray(0, maxBytes - scannedBytes);
      scannedBytes += data.byteLength;
      scannedFiles++;
      const text = new TextDecoder('utf-8', { fatal: false }).decode(data);
      truncated = scanText(e.name, text, hits) || truncated || hits.length >= maxHits;
      if (hits.length >= maxHits) {
        truncated = true;
        break;
      }
    }
  } catch (err) {
    // fail closed：连 zip 都读不成 ⇒ 拒收
    return { ok: false, hits, truncated, reason: `scan_failed:${err.message}`, scanned_files: scannedFiles, scanned_bytes: scannedBytes };
  }
  return { ok: hits.length === 0, hits, truncated, scanned_files: scannedFiles, scanned_bytes: scannedBytes };
}
