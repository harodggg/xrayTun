// @vitest-environment node
//
// 为什么强制 node 环境：这个测试只做一件事 —— 读 Rust 源码文本。jsdom 环境里
// `import.meta.url` 是 http://localhost 这种假 URL，`new URL('./contract.ts', import.meta.url)`
// 会直接抛「The URL must be of scheme file」；node 环境下它才是真实的 file:// 路径。
//
// 契约守卫 —— **文本级，不是类型级**。
//
// 它做什么：从 crates/xt-contract/src/{error,model,protocol}.rs 的源码文本里抽出
// struct 字段名、enum 成员名、带 tag 的枚举变体（tag + 字段名），再和 contract.ts 的文本
// 抽出来的同名结构逐一比对，断言 TS 侧没有**漏字段 / 多字段 / 漏枚举成员**。
//
// 它**不**证明的事（覆盖不到的边界，写出来而不是假装覆盖）：
//  1. 类型不对照：`String` ↔ `string`、`u64` ↔ `number`、`Option<serde_json::Value>` ↔ `unknown`
//     这类对应关系不在检查范围；字段可选性（serde 的 skip_serializing_if）也没查 ——
//     线上「字段缺失 = 不知道」的语义靠人工评审，不靠这个测试。
//  2. tuple(newtype) 变体的载荷形状查不了：`Response::Nodes(Vec<NodeView>)` 之类在 Rust 文本里
//     没有字段名，只能查 tag。注意它当前是 serde 的非法形状（internally tagged + 序列），
//     修成 struct 变体后本测试会自动开始检查它新增的字段名。
//  3. 它不认识 Rust 语法：用的是「大括号配对 + 顶层分隔符切分」的朴素解析，
//     宏生成的类型、嵌套复杂泛型、块注释都会让它失手 —— 失手时抛错，不静默通过。
//  4. 它不证明运行时真的按这些形状序列化（那是 Rust 侧 serde 测试的职责）。
//
// 另外两个显式映射（不是笔误）：
//  * Rust `Event` ↔ TS `DaemonEvent`（任务冻结清单里的名字）。
//  * Rust 的 tuple newtype（NodeId/SubscriptionId）线上就是裸字符串，TS 侧是 `string` 别名，
//    因此它们不在 struct 字段对照列表里，只在下面的 newtype 断言里出现。

import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';

// ------------------------------------------------------------------ 朴素解析工具

/** 去掉注释：整行 //（含 /// 与 //!）以及成对的块注释。契约文本里 `///` 说明文字很多，
 *  不先剥掉就会把「文档里提到的字段名」当成真字段。 */
function stripComments(source: string): string {
  return source
    .replace(/\/\*[\s\S]*?\*\//g, '')
    .split('\n')
    .filter((line) => !line.trim().startsWith('//'))
    .join('\n');
}

const CONTRACT_SRC = stripComments(readFileSync(new URL('./contract.ts', import.meta.url), 'utf8'));
const CLIENT_SRC = stripComments(readFileSync(new URL('./client.ts', import.meta.url), 'utf8'));
const ERROR_SRC = stripComments(readFileSync(new URL('../../../../crates/xt-contract/src/error.rs', import.meta.url), 'utf8'));
const MODEL_SRC = stripComments(readFileSync(new URL('../../../../crates/xt-contract/src/model.rs', import.meta.url), 'utf8'));
const PROTOCOL_SRC = stripComments(
  readFileSync(new URL('../../../../crates/xt-contract/src/protocol.rs', import.meta.url), 'utf8'),
);
const LIB_SRC = stripComments(readFileSync(new URL('../../../../crates/xt-contract/src/lib.rs', import.meta.url), 'utf8'));

/** 取 `header` 命中之后第一个大括号里的内容（大括号配对）。 */
function extractBody(source: string, header: RegExp): string {
  const match = header.exec(source);
  if (match === null) {
    throw new Error(`契约文本里找不到 ${String(header)} —— 类型被改名/删除了？`);
  }
  const start = source.indexOf('{', match.index);
  if (start < 0) throw new Error(`找不到 ${String(header)} 的左大括号`);
  let depth = 0;
  for (let i = start; i < source.length; i += 1) {
    const char = source[i];
    if (char === '{') depth += 1;
    if (char === '}') {
      depth -= 1;
      if (depth === 0) return source.slice(start + 1, i);
    }
  }
  throw new Error(`${String(header)} 的大括号不配对`);
}

/** 按分隔符切分，只切最外层（忽略括号里的分隔符）。 */
function splitTopLevel(text: string, separator: string): string[] {
  const parts: string[] = [];
  let depth = 0;
  let current = '';
  for (const char of text) {
    if (char === '{' || char === '(' || char === '[') depth += 1;
    if (char === '}' || char === ')' || char === ']') depth -= 1;
    if (char === separator && depth === 0) {
      parts.push(current);
      current = '';
      continue;
    }
    current += char;
  }
  parts.push(current);
  return parts.map((part) => part.trim()).filter((part) => part.length > 0);
}

function toSnakeCase(name: string): string {
  return name
    .replace(/([a-z0-9])([A-Z])/g, '$1_$2')
    .replace(/([A-Z]+)([A-Z][a-z])/g, '$1_$2')
    .toLowerCase();
}

// ------------------------------------------------------------------ Rust 侧抽取

function rustBracedStructs(source: string): string[] {
  return [...source.matchAll(/pub struct (\w+)\s*\{/g)].map((match) => match[1]);
}

function rustEnums(source: string): string[] {
  return [...source.matchAll(/pub enum (\w+)\s*\{/g)].map((match) => match[1]);
}

function rustStructFields(source: string, name: string): string[] {
  const body = extractBody(source, new RegExp(`pub struct ${name}\\s*\\{`));
  return [...body.matchAll(/^[ \t]*(?:pub[ \t]+)?(\w+)[ \t]*:/gm)].map((match) => match[1]);
}

interface RustVariant {
  name: string;
  /** `null` = tuple/newtype 载荷（文本里没有字段名）；`[]` = unit 变体。 */
  fields: string[] | null;
}

function rustEnumVariants(source: string, name: string): RustVariant[] {
  const rawBody = extractBody(source, new RegExp(`pub enum ${name}\\s*\\{`));
  // 变体上的属性（如 #[serde(...)]）不是变体名的一部分，先删掉。
  const body = rawBody
    .split('\n')
    .filter((line) => !line.trim().startsWith('#'))
    .join('\n');
  return splitTopLevel(body, ',').map((segment) => {
    const head = /^(\w+)/.exec(segment);
    if (head === null) throw new Error(`看不懂 ${name} 的枚举成员：${segment}`);
    const brace = segment.indexOf('{');
    if (brace >= 0) {
      const close = segment.lastIndexOf('}');
      const inner = segment.slice(brace + 1, close);
      const fields = splitTopLevel(inner, ',')
        .map((field) => /^[ \t]*(?:pub[ \t]+)?(\w+)[ \t]*:/.exec(field.trim()))
        .filter((match): match is RegExpExecArray => match !== null)
        .map((match) => match[1]);
      return { name: head[1], fields };
    }
    if (segment.includes('(')) return { name: head[1], fields: null };
    return { name: head[1], fields: [] };
  });
}

// ------------------------------------------------------------------ TS 侧抽取

function tsInterfaceFields(source: string, name: string): string[] {
  const body = extractBody(source, new RegExp(`export interface ${name}\\b`));
  return tsFieldsFromBody(body);
}

function tsFieldsFromBody(body: string): string[] {
  return splitTopLevel(body, ';')
    .map((segment) => /^[ \t]*(\w+)\??[ \t]*:/.exec(segment.trim()))
    .filter((match): match is RegExpExecArray => match !== null)
    .map((match) => match[1]);
}

function tsTypeAlias(source: string, name: string): string {
  const match = new RegExp(`export type ${name}\\b`).exec(source);
  if (match === null) throw new Error(`contract.ts 里找不到 export type ${name}`);
  const equals = source.indexOf('=', match.index);
  if (equals < 0) throw new Error(`export type ${name} 没有 =`);
  let depth = 0;
  for (let i = equals; i < source.length; i += 1) {
    const char = source[i];
    if (char === '{' || char === '(' || char === '[') depth += 1;
    if (char === '}' || char === ')' || char === ']') depth -= 1;
    if (char === ';' && depth === 0) return source.slice(equals + 1, i);
  }
  throw new Error(`export type ${name} 没有以顶层 ; 结束`);
}

function tsLiteralUnion(source: string, name: string): string[] {
  return [...tsTypeAlias(source, name).matchAll(/'([a-z_]+)'/g)].map((match) => match[1]);
}

function sorted(values: string[]): string[] {
  return [...values].sort();
}

// ------------------------------------------------------------------ 对照表

const MODEL_STRUCTS = [
  'ConnectionView',
  'DatapathView',
  'StatsView',
  'LogLine',
  'NodeView',
  'ProbeResult',
  'Notice',
  'SubscriptionView',
  'SettingsView',
  'SettingsPatch',
  'DaemonHello',
];

const MODEL_ENUMS = ['Stage', 'ConnectPhase', 'RunMode', 'LogLevel', 'NodeSource', 'NoticeSeverity', 'Capability', 'Topic'];

interface SimpleEnumCheck {
  rustSource: string;
  name: string;
  /** TS 侧的名字可能与 Rust 不同（目前只有 Event → DaemonEvent 这一处）。 */
  tsName?: string;
}

const SIMPLE_ENUMS: SimpleEnumCheck[] = [
  { rustSource: MODEL_SRC, name: 'Stage' },
  { rustSource: MODEL_SRC, name: 'ConnectPhase' },
  { rustSource: MODEL_SRC, name: 'RunMode' },
  { rustSource: MODEL_SRC, name: 'LogLevel' },
  { rustSource: MODEL_SRC, name: 'NoticeSeverity' },
  { rustSource: MODEL_SRC, name: 'Capability' },
  { rustSource: MODEL_SRC, name: 'Topic' },
  { rustSource: ERROR_SRC, name: 'ErrorCode' },
];

interface TaggedUnionCheck {
  rustSource: string;
  rustName: string;
  tsName: string;
  tag: string;
}

const TAGGED_UNIONS: TaggedUnionCheck[] = [
  { rustSource: MODEL_SRC, rustName: 'NodeSource', tsName: 'NodeSource', tag: 'kind' },
  { rustSource: PROTOCOL_SRC, rustName: 'Request', tsName: 'Request', tag: 'op' },
  { rustSource: PROTOCOL_SRC, rustName: 'Response', tsName: 'Response', tag: 'result' },
  { rustSource: PROTOCOL_SRC, rustName: 'Outcome', tsName: 'Outcome', tag: 'status' },
  { rustSource: PROTOCOL_SRC, rustName: 'Event', tsName: 'DaemonEvent', tag: 'event' },
  { rustSource: PROTOCOL_SRC, rustName: 'Frame', tsName: 'Frame', tag: 'kind' },
];

// ------------------------------------------------------------------ 用例

describe('xt-contract 文本级守卫', () => {
  it('struct 字段一一对应（不漏、不多）', () => {
    for (const name of MODEL_STRUCTS) {
      expect(sorted(tsInterfaceFields(CONTRACT_SRC, name)), `interface ${name}`).toEqual(
        sorted(rustStructFields(MODEL_SRC, name)),
      );
    }
    expect(sorted(tsInterfaceFields(CONTRACT_SRC, 'ErrorBody')), 'interface ErrorBody').toEqual(
      sorted(rustStructFields(ERROR_SRC, 'ErrorBody')),
    );
  });

  it('简单枚举成员一一对应', () => {
    for (const check of SIMPLE_ENUMS) {
      const rustValues = rustEnumVariants(check.rustSource, check.name);
      for (const variant of rustValues) {
        // 这些枚举必须全是 unit 变体；出现带载荷的成员说明契约变了，守卫要跟着改。
        expect(variant.fields, `${check.name}::${variant.name} 不应带载荷`).toEqual([]);
      }
      expect(sorted(tsLiteralUnion(CONTRACT_SRC, check.tsName ?? check.name)), `${check.name} 的字面量`).toEqual(
        sorted(rustValues.map((variant) => toSnakeCase(variant.name))),
      );
    }
    // lead 追加的成员：能力未宣告的唯一合法表达。少了它，UI 无法如实表达「本版本不做这个能力」。
    expect(tsLiteralUnion(CONTRACT_SRC, 'ErrorCode')).toContain('unsupported');
  });

  it('ErrorCode 的封闭集合在 client.ts 里也是同一份（否则运行时会判错错误类型）', () => {
    const block = /const ERROR_CODES: readonly string\[\] = \[([\s\S]*?)\];/.exec(CLIENT_SRC);
    expect(block, 'client.ts 里找不到 ERROR_CODES').not.toBeNull();
    const clientCodes = [...block![1].matchAll(/'([a-z_]+)'/g)].map((match) => match[1]);
    expect(sorted(clientCodes), 'client.ts 的 ERROR_CODES').toEqual(
      sorted(rustEnumVariants(ERROR_SRC, 'ErrorCode').map((variant) => toSnakeCase(variant.name))),
    );
  });

  it('list_nodes / list_subscriptions / tail_logs 的载荷键名与 Rust struct 变体逐一对应', () => {
    // 这三条曾经是 serde 的非法形状（内部 tag + newtype 包序列），修好后的形状是
    // `Nodes { nodes }` / `Subscriptions { subscriptions }` / `Logs { logs }`。
    // 单独写一条是因为「键名写错」不会让类型报错（`{result:'nodes'; nodes:...}` 少了键、
    // 或者 Rust 又改回 tuple 变体），只会在运行时变成 internal 错误。
    const expected: [string, string][] = [
      ['Nodes', 'nodes'],
      ['Subscriptions', 'subscriptions'],
      ['Logs', 'logs'],
    ];
    const members = splitTopLevel(tsTypeAlias(CONTRACT_SRC, 'Response'), '|');
    for (const [rustVariant, key] of expected) {
      const variant = rustEnumVariants(PROTOCOL_SRC, 'Response').find((candidate) => candidate.name === rustVariant);
      expect(variant, `Rust Response::${rustVariant}`).toBeDefined();
      expect(variant!.fields, `Rust Response::${rustVariant} 必须是结构体变体`).toEqual([key]);
      const member = members.find((candidate) => candidate.includes(`result: '${toSnakeCase(rustVariant)}'`));
      expect(member, `TS Response 里缺少 result = ${toSnakeCase(rustVariant)}`).toBeDefined();
      const inner = member!.slice(member!.indexOf('{') + 1, member!.lastIndexOf('}'));
      expect(tsFieldsFromBody(inner).filter((field) => field !== 'result'), `TS Response.${key}`).toEqual([key]);
    }
  });

  it('带 tag 的联合：tag 值与 struct 变体的字段一一对应', () => {
    for (const check of TAGGED_UNIONS) {
      const variants = rustEnumVariants(check.rustSource, check.rustName);
      const members = splitTopLevel(tsTypeAlias(CONTRACT_SRC, check.tsName), '|');
      const tagPattern = new RegExp(`${check.tag}:\\s*'([a-z_]+)'`);
      const tsTags = members.map((member) => {
        const match = tagPattern.exec(member);
        if (match === null) throw new Error(`TS ${check.tsName} 的成员里找不到 ${check.tag}：${member}`);
        return match[1];
      });
      expect(sorted(tsTags), `${check.tsName} 的 ${check.tag} 值`).toEqual(
        sorted(variants.map((variant) => toSnakeCase(variant.name))),
      );

      for (const variant of variants) {
        if (variant.fields === null) continue; // 边界 2：tuple/newtype 载荷查不了
        const tagValue = toSnakeCase(variant.name);
        const member = members.find((candidate) => tagPattern.test(candidate) && tagPattern.exec(candidate)![1] === tagValue);
        if (member === undefined) throw new Error(`TS ${check.tsName} 缺少 ${check.tag} = ${tagValue}`);
        const inner = member.slice(member.indexOf('{') + 1, member.lastIndexOf('}'));
        const tsFields = tsFieldsFromBody(inner).filter((field) => field !== check.tag);
        expect(sorted(tsFields), `${check.tsName}.${tagValue}`).toEqual(sorted(variant.fields));
      }
    }
  });

  it('newtype 在线上是裸字符串；帧 id / seq 是 u64 → number', () => {
    expect(MODEL_SRC).toMatch(/pub struct NodeId\(String\);/);
    expect(MODEL_SRC).toMatch(/pub struct SubscriptionId\(String\);/);
    expect(CONTRACT_SRC).toMatch(/export type NodeId = string;/);
    expect(CONTRACT_SRC).toMatch(/export type SubscriptionId = string;/);

    expect(PROTOCOL_SRC).toMatch(/pub type RequestId = u64;/);
    expect(PROTOCOL_SRC).toMatch(/pub type EventSeq = u64;/);
    expect(CONTRACT_SRC).toMatch(/export type RequestId = number;/);
    expect(CONTRACT_SRC).toMatch(/export type EventSeq = number;/);
  });

  it('协议版本与帧上限常量一致', () => {
    const version = /pub const PROTOCOL_VERSION: u32 = (\d+);/.exec(LIB_SRC);
    const maxFrame = /pub const MAX_FRAME_BYTES: u32 = ([0-9 *]+);/.exec(LIB_SRC.replace(/\s+/g, ' '));
    expect(version, 'PROTOCOL_VERSION 没找到').not.toBeNull();
    expect(maxFrame, 'MAX_FRAME_BYTES 没找到').not.toBeNull();
    expect(CONTRACT_SRC).toContain(`export const PROTOCOL_VERSION = ${version![1]};`);
    // Rust 侧写成 `1024 * 1024`；这里只断言两边都是同一个算式结果，不比对字面量写法。
    const rustMaxFrame = maxFrame![1]
      .split('*')
      .map((part) => Number(part.trim()))
      .reduce((left, right) => left * right, 1);
    const tsMaxFrame = /export const MAX_FRAME_BYTES = ([0-9 *]+);/.exec(CONTRACT_SRC);
    expect(tsMaxFrame, 'TS 侧 MAX_FRAME_BYTES 没找到').not.toBeNull();
    const tsValue = tsMaxFrame![1]
      .split('*')
      .map((part) => Number(part.trim()))
      .reduce((left, right) => left * right, 1);
    expect(tsValue).toBe(rustMaxFrame);
  });

  it('覆盖度：契约里每个 braced struct / enum 都必须在本守卫的对照表里', () => {
    // 这条用来防止「Rust 加了新类型，前端守卫悄悄漏掉」——
    // 漏掉时这个测试会红，而不是安静地什么都不查。
    expect(sorted(rustBracedStructs(MODEL_SRC))).toEqual(sorted(MODEL_STRUCTS));
    expect(sorted(rustEnums(MODEL_SRC))).toEqual(sorted(MODEL_ENUMS));
    expect(sorted(rustBracedStructs(ERROR_SRC))).toEqual(['ErrorBody']);
    expect(sorted(rustEnums(ERROR_SRC))).toEqual(['ErrorCode']);
    expect(sorted(rustEnums(PROTOCOL_SRC))).toEqual(sorted(TAGGED_UNIONS.filter((check) => check.rustSource === PROTOCOL_SRC).map((check) => check.rustName)));
  });
});
