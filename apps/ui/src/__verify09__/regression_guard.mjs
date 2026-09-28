#!/usr/bin/env node
// 0.9.0 独立验证 guard（只读产品文件，不修改任何东西）。
//
//   node apps/ui/src/__verify09__/regression_guard.mjs
//
// 覆盖：
//   明确不做 1..10（0.9-PLAN.md §3「明确不做」）
//   回归项 11 条（0.9-PLAN.md §3.0 表：已落地，只许核对不许重做）
//
// 证据级别声明：本脚本是 **结构/文本级（L3）** 判据，只能证明"那段必要复杂度还在"，
// 不能证明行为正确；行为级证据以现有 vitest（useFollowScroll / singleLineButton /
// destructiveConfirm / logsDomStability 等）为准，两者互补。
//
// 退出码：0 = 全部通过；N>0 = 失败条数。

import { readFileSync, existsSync, readdirSync, statSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, join, resolve } from 'node:path';

const HERE = dirname(fileURLToPath(import.meta.url));
const ROOT = resolve(HERE, '..', '..', '..', '..');   // xray-tun/
const UI = join(ROOT, 'apps', 'ui');
const DESKTOP = join(ROOT, 'apps', 'desktop');

let PASS = 0, FAIL = 0;
const results = [];

function read(rel) {
  const p = join(ROOT, rel);
  if (!existsSync(p) || statSync(p).isDirectory()) return null;
  return readFileSync(p, 'utf8');
}
function grepTree(dirRel, ext, re) {
  const base = join(ROOT, dirRel);
  const out = [];
  if (!existsSync(base)) return out;
  for (const f of readdirSync(base, { recursive: true })) {
    const rel = String(f);
    if (!rel.endsWith(ext)) continue;
    const p = join(base, rel);
    if (!statSync(p).isFile()) continue;
    readFileSync(p, 'utf8').split('\n').forEach((l, i) => {
      if (re.test(l)) out.push(`${dirRel}/${rel}:${i + 1}: ${l.trim().slice(0, 120)}`);
    });
  }
  return out;
}
function lines(rel) { const t = read(rel); return t === null ? null : t.split('\n'); }
function hits(rel, re) {
  const ls = lines(rel);
  if (!ls) return null;
  const out = [];
  ls.forEach((l, i) => { if (re.test(l)) out.push(`${rel}:${i + 1}: ${l.trim().slice(0, 130)}`); });
  return out;
}
function check(label, ok, evidence = []) {
  results.push({ label, ok });
  if (ok) { PASS++; console.log(`  [PASS] ${label}`); }
  else { FAIL++; console.log(`  [FAIL] ${label}`); }
  for (const e of (Array.isArray(evidence) ? evidence : [evidence]).filter(Boolean).slice(0, 6)) {
    console.log(`         ${e}`);
  }
}

console.log('0.9.0 regression guard（结构/文本级 L3）');
console.log(`ROOT=${ROOT}\n`);

// ============================================================ 明确不做 1..10
console.log('== 明确不做 1：两阶段启动顺序 / supervisor 两次可达性检查 ==');
{
  const sup = read('apps/desktop/src/supervisor.rs') || '';
  const retry = /tcp_reachable_with_retry/.test(sup);
  const twoProbes = /tcp_reachable_with_retry_by/.test(sup) && /probe\(\)\.await/.test(sup);
  const trace = /两次的轨迹|traces|轨迹/.test(sup);
  check('supervisor 仍做「失败再试一次」的两次探测', retry && twoProbes,
    hits('apps/desktop/src/supervisor.rs', /tcp_reachable_with_retry|probe\(\)/));
  check('两次探测的轨迹仍被保留（不是静默重试）', trace, hits('apps/desktop/src/supervisor.rs', /轨迹/));
  const phase = [
    ...(hits('apps/desktop/src/state.rs', /两阶段启动/) || []),
    ...(hits('apps/desktop/src/supervisor.rs', /启动顺序/) || []),
    ...(hits('crates/xt-proto/src/lib.rs', /两阶段启动/) || []),
    ...(hits('crates/xt-helper/src/server.rs', /两阶段启动/) || []),
  ];
  check('两阶段启动注释/结构仍在（state.rs / supervisor.rs / xt-proto）', phase.length > 0, phase);
}

console.log('\n== 明确不做 2：helper 会话快照 / restore_stale / /1 路由 / 尽力而为回滚 ==');
{
  const h = read('apps/desktop/src/commands/helper.rs') || '';
  const lib = read('apps/desktop/src/lib.rs') || '';
  check('restore_stale 仍存在且已注册', /pub async fn restore_stale/.test(h) && /restore_stale/.test(lib),
    hits('apps/desktop/src/commands/helper.rs', /restore_stale/));
  // 「/1 路由拆分」= 默认路由不替换，而是加 0.0.0.0/1 + 128.0.0.0/1（v6: ::/1 + 8000::/1）
  const proto = read('crates/xt-proto/src/lib.rs') || '';
  const plan = read('crates/xt-tun/src/plan.rs') || '';
  const v4split = /0\.0\.0\.0\/1/.test(proto + plan) && /128\.0\.0\.0\/1/.test(proto + plan);
  const v6split = /::\/1/.test(proto + plan) && /8000::\/1/.test(proto + plan);
  check('/1 路由拆分仍在（v4 双 /1）', v4split,
    hits('crates/xt-proto/src/lib.rs', /0\.0\.0\.0\/1|128\.0\.0\.0\/1/));
  check('/1 路由拆分仍在（v6 双 /1）', v6split,
    hits('crates/xt-proto/src/lib.rs', /::\/1|8000::\/1/));
  const best = [
    ...(hits('apps/desktop/src/commands/core.rs', /回滚失败/) || []),
    ...(hits('apps/desktop/src/commands/core.rs', /未能确认|DirectUnverified/) || []),
  ];
  check('尽力而为回滚 + 不编好消息的措辞仍在（回滚失败 ⇒ 未知/未确认）', best.length >= 2, best);
}

console.log('\n== 明确不做 3：看门狗 10s + 连续失败重建 + MonitorGuard pid 去重 + slept_for ==');
{
  const state = read('apps/desktop/src/state.rs') || '';
  const nh = read('apps/desktop/src/node_health.rs') || '';
  check('看门狗仍是固定 10 秒节拍', /固定 10 秒|10 秒一跳|10s/.test(state + nh),
    hits('apps/desktop/src/state.rs', /10 秒/));
  const core = read('apps/desktop/src/commands/core.rs') || '';
  check('MonitorGuard（按 pid 去重）仍在', /MonitorGuard/.test(state + nh + core),
    hits('apps/desktop/src/commands/core.rs', /MonitorGuard/));
  check('slept_for 仍在（睡眠唤醒判定）', /pub\(crate\) fn slept_for/.test(core),
    hits('apps/desktop/src/commands/core.rs', /fn slept_for/));
  const streak = hits('apps/desktop/src/node_health.rs', /CONSECUTIVE_FAILURES|连续失败多少次/);
  check('连续失败阈值仍在（重建/提示判据）', Array.isArray(streak) && streak.length > 0, streak);
}

console.log('\n== 明确不做 4：was_connected（意图）与 runtime.running（观测）分离 ==');
{
  const src = ['apps/desktop/src/lib.rs', 'apps/desktop/src/state.rs', 'apps/desktop/src/intent.rs']
    .map(read).filter(Boolean).join('\n');
  const ui = [read('apps/ui/src/store.tsx'), read('apps/ui/src/types.ts')].filter(Boolean).join('\n');
  const hasIntent = /was_connected/.test(src) || /was_connected/.test(ui);
  const hasObs = /running/.test(src);
  check('was_connected（意图）仍在', hasIntent,
    hits('apps/desktop/src/lib.rs', /was_connected/) || hits('apps/ui/src/types.ts', /was_connected/));
  check('runtime.running（观测）仍在', hasObs, hits('apps/desktop/src/lib.rs', /running/));
}

console.log('\n== 明确不做 5：设置页「只渲染当前分类」而非 CSS 隐藏 ==');
{
  const st = read('apps/ui/src/pages/Settings.tsx') || '';
  const rendered = /activeCategory|activeCat|currentCategory|cat ===|category ===/.test(st);
  check('Settings.tsx 仍有「当前分类」条件渲染', rendered,
    hits('apps/ui/src/pages/Settings.tsx', /activeCategory|cat ===|category ===/));
  const hide = hits('apps/ui/src/styles.css', /\.set__panel[^{]*\{[^}]*display:\s*none/);
  check('没有把分类面板改成 CSS display:none 隐藏', !hide || hide.length === 0, hide || '(0 hit)');
}

console.log('\n== 明确不做 6：跟随滚动的「往上翻暂停」规则 ==');
{
  const f = read('apps/ui/src/useFollowScroll.ts') || '';
  check('useFollowScroll 仍是往上翻暂停的实现（非 CSS 贴底）',
    /pause|暂停|scrollTop/.test(f) && !/scroll-behavior:\s*smooth/.test(read('apps/ui/src/styles.css')),
    hits('apps/ui/src/useFollowScroll.ts', /暂停|pause|scrollTop/));
  check('行为级测试文件仍在', existsSync(join(UI, 'src', 'useFollowScroll.test.tsx')),
    'apps/ui/src/useFollowScroll.test.tsx');
}

console.log('\n== 明确不做 7：确定性步进（不要 CSS 动画） —— Lead 主责，这里只记录 ==');
{
  const flow = read('apps/ui/src/topology/Flow.tsx') || '';
  check('Flow.tsx 仍用 requestAnimationFrame 步进（不是 CSS 动画）',
    /requestAnimationFrame/.test(flow), hits('apps/ui/src/topology/Flow.tsx', /requestAnimationFrame/));
  const carKeyframes = hits('apps/ui/src/styles.css', /@keyframes\s+\S*(car|flow|topo)/i);
  check('没有新增「拓扑车」CSS keyframes', !carKeyframes || carKeyframes.length === 0,
    carKeyframes || '(0 hit)');
}

console.log('\n== 明确不做 8：canvas 地球仪 / getPointAtLength / --status-off / 五档状态色 ==');
{
  check('canvas 地球仪仍在', /<canvas|getContext\(/.test(read('apps/ui/src/pages/Globe.tsx') || ''),
    hits('apps/ui/src/pages/Globe.tsx', /canvas|getContext/));
  const sample = hits('apps/ui/src/topology/Flow.tsx', /getPointAtLength/);
  check('getPointAtLength 采样仍在', Array.isArray(sample) && sample.length > 0, sample);
  const css = read('apps/ui/src/styles.css') || '';
  check('--status-off / --status-on 仍在', /--status-off:/.test(css) && /--status-on:/.test(css),
    hits('apps/ui/src/styles.css', /--status-(on|off):/));
  const tiers = (css.match(/--status-/g) || []).length;
  check('五档状态色注释/变量仍在（>=5 处 --status-）', tiers >= 5, `--status- 出现 ${tiers} 次`);
  check('「绿色只属于 on」的约束仍在', /绿色只属于|绿色.*只.*on|--status-off.*不是绿|中性灰/.test(css),
    hits('apps/ui/src/styles.css', /绿色只属于|中性灰/));
}

console.log('\n== 明确不做 9：诚实三做不到 ==');
{
  const dash = read('apps/ui/src/pages/Dashboard.tsx') || '';
  const nodes = read('apps/ui/src/pages/Nodes.tsx') || '';
  const nodes2 = read('apps/ui/src/pages/Nodes.tsx') || '';
  const topo = read('apps/ui/src/pages/Topology.tsx') || '';
  const conn = /连接数/.test(dash + nodes2 + topo);
  const internalListed = /内部通道/.test(topo) && /分开列|没有连线|不画 0 字节/.test(topo + read('apps/ui/src/ia090Topology.test.tsx'));
  check('① 内部通道不假装有字节/连线（分开列 + 连接数，而不是编造流量）', conn || internalListed,
    [...(hits('apps/ui/src/pages/Topology.tsx', /内部通道|分开列|没有连线/) || []),
     ...(hits('apps/ui/src/pages/Dashboard.tsx', /连接数/) || []),
     ...(hits('apps/ui/src/pages/Nodes.tsx', /连接数/) || [])]);
  check('② 字节不可用显示「不可用」', /不可用/.test(dash + nodes),
    hits('apps/ui/src/pages/Dashboard.tsx', /不可用/));
  const star = hits('apps/ui/src/pages/Routing.tsx', /\*/);
  check('③ 域名 `*` 语义仍在（规则页星号说明）', Array.isArray(star) && star.length > 0, star);
}

console.log('\n== 明确不做 10：破坏性 = 二次确认（不是假撤销）；高频动作不加确认 ==');
{
  const inline = hits('apps/ui/src/InlineConfirm.tsx', /InlineConfirm|确认/);
  check('InlineConfirm 组件仍在', Array.isArray(inline) && inline.length > 0, inline);
  for (const [file, tag] of [
    ['apps/ui/src/pages/Settings.tsx', 'Settings'],
    ['apps/ui/src/pages/Subscriptions.tsx', 'Subscriptions'],
    ['apps/ui/src/pages/Nodes.tsx', 'Nodes'],
    ['apps/ui/src/pages/Logs.tsx', 'Logs'],
  ]) {
    const h = hits(file, /InlineConfirm/);
    check(`${tag} 的破坏性操作仍走 InlineConfirm`, Array.isArray(h) && h.length > 0, h);
  }
  const nodes = read('apps/ui/src/pages/Nodes.tsx') || '';
  const switchFn = /switchNode|handleSwitch|selectNode/.exec(nodes);
  const noConfirmOnSwitch = !/switchNode[\s\S]{0,400}InlineConfirm/.test(nodes);
  check('换节点（高频）没有被加上二次确认', noConfirmOnSwitch,
    switchFn ? `Nodes.tsx switch fn at ${nodes.slice(0, switchFn.index).split('\n').length}` : '(no switch fn found)');
}

console.log('\n== P0·E1r：残留冗余 gap:8（判据：宿主确为 .row 才算冗余） ==');
{
  const offenders = [];
  for (const rel of ['apps/ui/src/pages/Settings.tsx', 'apps/ui/src/pages/Routing.tsx',
                     'apps/ui/src/pages/Nodes.tsx']) {
    const t = read(rel);
    if (!t) continue;
    for (const m of t.matchAll(/gap:\s*8\b/g)) {
      const before = t.slice(Math.max(0, m.index - 320), m.index);
      const cls = [...before.matchAll(/className=(?:"([^"]*)"|\{`([^`]*)`\})/g)].pop();
      const clsText = cls ? (cls[1] ?? cls[2] ?? '') : '';
      if (/\brow\b/.test(clsText)) offenders.push(`${rel}: ${clsText}`);
    }
  }
  check('没有「宿主已是 .row 还再写 gap:8」的冗余', offenders.length === 0,
    offenders.length ? offenders : ['（0 处：唯一残留的 gap:8 在 Routing.tsx:515 的普通 flex div 上，不属 .row）']);
}

// ============================================================ 回归项（§3.0）
console.log('\n== 回归项 11 条（只核「未被破坏」） ==');
{
  const core = read('apps/desktop/src/commands/core.rs') || '';
  check('R1 回滚不撒谎：core.rs 仍有 !…「网络可用」断言', /!\s*\w*\.?contains\(\s*"网络可用"/.test(core) || /assert!\([^)]*网络可用/.test(core),
    hits('apps/desktop/src/commands/core.rs', /网络可用/).slice(0, 4));
  check('R2 snapshot spawn 去重注释/实现仍在', /原来算两遍/.test(read('apps/desktop/src/commands/snapshot.rs') || ''),
    hits('apps/desktop/src/commands/snapshot.rs', /原来算两遍/));
  check('R3 分流重连提示仍在', /重新连接|立即重连/.test(read('apps/ui/src/pages/Routing.tsx') || ''),
    hits('apps/ui/src/pages/Routing.tsx', /重新连接|立即重连/));
  check('R5 CopyButton 仍给 role=status/alert', /role="status"|role="alert"/.test(read('apps/ui/src/pages/Logs.tsx') || ''),
    hits('apps/ui/src/pages/Logs.tsx', /role="(status|alert)"/));
  check('R6 auto_reconnect 开关仍在', /auto_reconnect/.test(read('apps/ui/src/pages/Settings.tsx') || ''),
    hits('apps/ui/src/pages/Settings.tsx', /auto_reconnect/));
  check('R7 TUN 高级折叠仍在', /高级/.test(read('apps/ui/src/pages/Settings.tsx') || ''),
    hits('apps/ui/src/pages/Settings.tsx', /高级/));
  check('R8 顶栏唯一主操作测试仍在', existsSync(join(UI, 'src', 'singleRunButton.test.tsx')),
    'apps/ui/src/singleRunButton.test.tsx');
  {
    // 按 bundle 作用域：只看应用自己的 styles.css，且先剥掉注释
    // （文件自己 :1383 警告过"全局 token 集合"会让这条判据失效）。
    const raw = read('apps/ui/src/styles.css') || '';
    const css = raw.replace(/\/\*[\s\S]*?\*\//g, '');
    const defined = new Set([...css.matchAll(/(--[a-zA-Z0-9_-]+)\s*:/g)].map((m) => m[1]));
    const bad = new Map();
    for (const m of css.matchAll(/var\((--[a-zA-Z0-9_-]+)(\s*,)?/g)) {
      const name = m[1];
      if (m[2]) continue;                       // has a fallback -> invalid-at-computed-time cannot bite
      if (!defined.has(name)) bad.set(name, (bad.get(name) || 0) + 1);
    }
    const detail = [...bad.entries()].map(([k, v]) => `var(${k}) 被引用 ${v} 次但无定义且无兜底`);
    check('R9 styles.css 没有被引用但未定义的 token（独立重算，忽略带兜底的 var）', bad.size === 0,
      detail.length ? detail : [`defined tokens=${defined.size}, undefined-used=${bad.size}`]);
  }

  // R10：独立重算 styles.css 行数与 40 行窗口重复（不采信任何自述）
  const cssLines = lines('apps/ui/src/styles.css');
  console.log(`  [OBS] styles.css 行数 = ${cssLines ? cssLines.length : 'MISSING'}`);
  if (cssLines) {
    const W = 40;
    const seen = new Map();
    let dupGroups = 0;
    const examples = [];
    for (let i = 0; i + W <= cssLines.length; i++) {
      const key = cssLines.slice(i, i + W).join('\n');
      if (seen.has(key)) { dupGroups++; if (examples.length < 3) examples.push(`窗口 ${seen.get(key) + 1} 与 ${i + 1} 完全相同`); }
      else seen.set(key, i);
    }
    check('R10 styles.css 无 40 行逐字节重复窗口（独立重算）', dupGroups === 0,
      [`重复窗口组数=${dupGroups}`, ...examples]);
  } else {
    check('R10 styles.css 存在', false, 'MISSING');
  }

  const css = read('apps/ui/src/styles.css') || '';
  const rm = /prefers-reduced-motion/.test(css);
  const rmNames = ['spin', 'set-target-flash', 'conn-dash'].filter((n) => new RegExp(n).test(css));
  check('R11 prefers-reduced-motion 仍在且覆盖三个动画名', rm && rmNames.length === 3,
    [`prefers-reduced-motion=${rm}`, `animations found=${rmNames.join(',')}`]);
}

console.log(`\n---- regression_guard: PASS=${PASS} FAIL=${FAIL} ----`);
for (const r of results.filter((r) => !r.ok)) console.log(`  FAIL: ${r.label}`);
process.exit(FAIL);
