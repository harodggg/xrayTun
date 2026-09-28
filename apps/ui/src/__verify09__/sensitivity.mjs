#!/usr/bin/env node
/**
 * 0.9.0 敏感性实验（verifier-0.9 / task-9）：故意破坏一处判据 → 必须红 → 恢复 → 绿。
 *
 * 做法：**不修改产品文件**。把目标文件复制到 `__verify09__/mutants/`，在副本上做
 * 一处精确替换（替换前断言 needle 唯一），再用 `mutant.vitest.config.ts` 的 alias
 * 让产品测试跑在副本上。每跑完一个 mutant，立刻用**原件**跑同一条测试做对照。
 *
 *   node apps/ui/src/__verify09__/sensitivity.mjs
 *
 * 覆盖 task-9 指定的 5 条：B1 自动保存 / B3 换节点提示 / D1 线型编码 /
 * D2 等级字符 / F1 空态主按钮。
 *
 * 原始输出写到 `__verify09__/out/sens-<案件>-{mutant,original}.txt`。
 * 退出码：失败的案件数（0 = 5/5：mutant 红、原件绿）。
 */
import { execFileSync } from "node:child_process";
import { mkdirSync, readFileSync, writeFileSync, existsSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join, resolve } from "node:path";

/**
 * mutant 被放在 `__verify09__/mutants/`，它的**相对 import 会解析到错误位置**
 * （`../ipc` 会去找 `__verify09__/ipc`）⇒ 模块加载失败 ⇒ vitest 报 "no tests"。
 * 那种红是**假红**，不是"判据有牙"。所以这里把 mutant 里所有相对 import
 * 重写成**指向真实文件的绝对路径**（以原文件位置为基准解析）。
 */
function rewriteImports(code, originalAbsPath) {
  const dir = dirname(originalAbsPath);
  return code.replace(/from\s+"(\.[^"]+)"/g, (whole, spec) => {
    const base = resolve(dir, spec);
    for (const ext of ["", ".ts", ".tsx", ".js", ".jsx"]) {
      if (existsSync(base + ext)) return `from "${base + ext}"`;
    }
    return whole;
  });
}

const HERE = dirname(fileURLToPath(import.meta.url));
const UI = resolve(HERE, "..", "..");           // apps/ui
const MUTANTS = join(HERE, "mutants");
const OUT = join(HERE, "out");
mkdirSync(MUTANTS, { recursive: true });
mkdirSync(OUT, { recursive: true });

const PROBE = "src/__verify09__/p0_probe.test.tsx";
const CONFIG = "src/__verify09__/mutant.vitest.config.ts";

// 先做「alias 真的生效」校准：一个**故意写坏**的副本必须让测试跑不起来。
// 否则 5 个案件全绿会是一种假象（测试根本没跑在 mutant 上）。
function calibrate() {
  const src = readFileSync(join(UI, "src/pages/Settings.tsx"), "utf8");
  const broken = join(MUTANTS, "calibration-broken.tsx");
  writeFileSync(broken, rewriteImports(src + "\nconst VERIFY09_CALIBRATION: = ;\n", join(UI, "src/pages/Settings.tsx")));
  const r = runVitest({ filter: "拨一下开关就落盘", find: "^\\.\\./pages/Settings$", mutant: broken });
  writeFileSync(join(OUT, "sens-calibration.txt"), r.out);
  return r.rc !== 0;
}

const CASES = [
  {
    id: "S1-b1-autosave",
    find: "^\\.\\./pages/Settings$",
    label: "B1 自动保存",
    target: "src/pages/Settings.tsx",
    needle: 'const ok = await runQueued("save", () => api.saveSettings(value));',
    replace: "const ok = true; // VERIFY09 MUTANT: 不再落盘",
    filter: "拨一下开关就落盘",
  },
  {
    id: "S2-b3-switch-notice",
    find: "^\\.\\./pages/Nodes$",
    label: "B3 换节点进行中提示",
    target: "src/pages/Nodes.tsx",
    needle: "setSwitchingTo(node.name);",
    replace: "setSwitchingTo(null); // VERIFY09 MUTANT: 不显示目标节点",
    filter: "正在切换到",
  },
  {
    id: "S3-d1-dasharray",
    find: "^\\./Flow$",
    label: "D1 线型第二编码（渲染路径）",
    target: "src/topology/Flow.tsx",
    needle: "strokeDasharray={sg.outKind ? OUTBOUND_DASH[sg.outKind] : undefined}",
    replace: "strokeDasharray={undefined} // VERIFY09 MUTANT: 去掉渲染路径线型",
    filter: "渲染路径",
  },
  {
    id: "S4-d2-level-char",
    find: "^\\.\\./pages/Logs$",
    label: "D2 日志等级字符",
    target: "src/pages/Logs.tsx",
    needle: '{`${LEVEL_CHAR[line.level] ?? "·"} ${formatClock(line.ts_unix)}`}',
    replace: "{`${formatClock(line.ts_unix)}`} /* VERIFY09 MUTANT: 去掉等级字符 */",
    filter: "warn/error 行分别带",
  },
  {
    id: "S5-f1-empty-cta",
    find: "^\\.\\./pages/Dashboard$",
    label: "F1 空态主按钮",
    target: "src/pages/Dashboard.tsx",
    needle: '{nodes.length === 0 ? "添加订阅" : connected ? "切换节点" : "选择节点"}',
    replace: '{"选择节点"} /* VERIFY09 MUTANT: 空态不再是添加订阅 */',
    filter: "零节点时按钮是",
  },
];

function runVitest({ filter, find, mutant }) {
  const env = { ...process.env };
  if (find && mutant) {
    env.VERIFY09_FIND = find;
    env.VERIFY09_MUTANT = mutant;
  } else {
    delete env.VERIFY09_FIND;
    delete env.VERIFY09_MUTANT;
  }
  try {
    const out = execFileSync(
      "npx",
      ["vitest", "run", PROBE, "--config", CONFIG, "-t", filter],
      { cwd: UI, env, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"], timeout: 300000 },
    );
    return { rc: 0, out };
  } catch (e) {
    return { rc: e.status ?? 1, out: `${e.stdout ?? ""}\n${e.stderr ?? ""}` };
  }
}

let failed = 0;
console.log("0.9.0 敏感性实验（mutant 用副本 + alias，不改产品文件）\n");
if (!calibrate()) {
  console.log("  [FATAL] alias 校准失败：写坏的副本没有让测试变红 ⇒ alias 没生效，5 条结论全部作废");
  process.exit(99);
}
console.log("  [PASS] 校准：写坏的 Settings 副本让测试变红 ⇒ alias 确实作用在 mutant 上\n");
for (const c of CASES) {
  const srcPath = join(UI, c.target);
  const src = readFileSync(srcPath, "utf8");
  const count = src.split(c.needle).length - 1;
  if (count !== 1) {
    failed++;
    console.log(`  [FAIL] ${c.id}: 锚点在 ${c.target} 出现 ${count} 次（必须恰好 1 次）——突变无效，结论作废`);
    continue;
  }
  const mutantPath = join(MUTANTS, `${c.id}${c.target.slice(c.target.lastIndexOf("."))}`);
  writeFileSync(mutantPath, rewriteImports(src.replace(c.needle, c.replace), srcPath));

  const m = runVitest({ filter: c.filter, find: c.find, mutant: mutantPath });
  const o = runVitest({ filter: c.filter, find: null, mutant: null });
  writeFileSync(join(OUT, `sens-${c.id}-mutant.txt`), m.out);
  writeFileSync(join(OUT, `sens-${c.id}-original.txt`), o.out);

  // 红必须是「某条断言失败」，不是「模块没加载起来」——后者是假红。
  const realFailure = /Tests\s+\d+ failed/.test(m.out) && !/Tests\s+no tests/.test(m.out);
  const ok = m.rc !== 0 && realFailure && o.rc === 0;
  if (!ok) failed++;
  if (m.rc === 0) console.log("         诊断：mutant 没红 —— 先怀疑 alias 没作用到该模块，不要当成'判据没牙'");
  if (m.rc !== 0 && !realFailure) {
    console.log("         诊断：mutant 红了但**没有断言失败**（很可能是模块加载错误=假红）——本次结论作废");
    console.log("         mutant 输出摘录：" + (m.out.match(/(Error|error)[^\n]*/) ?? ["(none)"])[0]);
  }
  console.log(
    `  [${ok ? "PASS" : "FAIL"}] ${c.id} · ${c.label}: mutant rc=${m.rc}（应≠0） original rc=${o.rc}（应=0）`,
  );
  const mu = (m.out.match(/Tests\s+.*/) ?? ["(no summary)"])[0].trim();
  const or = (o.out.match(/Tests\s+.*/) ?? ["(no summary)"])[0].trim();
  console.log(`         mutant: ${mu}`);
  console.log(`         original: ${or}`);
}

console.log(`\n---- sensitivity: ${CASES.length - failed}/${CASES.length} 案件通过 ----`);
console.log(`原始输出：${OUT}/sens-*.txt`);
process.exit(failed);
