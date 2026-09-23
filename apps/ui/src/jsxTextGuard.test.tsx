/**
 * JSX 文本节点守卫（task-150）。
 *
 * # 为什么要有这条
 *
 * `task-142` 里发现 **5 处**把 markdown 的 `**强调**` 直接写进 JSX 文本，用户在界面上
 * 看到的是 `**真的在探测**` 这种带星号的原文。它们能活下来，是因为那几张卡的断言是
 * `toContain("真的在探测")` —— 而 `**真的在探测**` **正好包含**这个子串：
 * **断言比它看起来弱**，缺陷从断言下面穿过去了。
 *
 * ⇒ 这一类不该靠「下一个人再想起来扫一遍」。这条守卫让它在**任何文件**都过不去。
 *
 * # 扫描器怎么工作（轻量、无依赖）
 *
 * 逐行扫 `*.tsx`，对每一行先做**排除**，再看剩下的部分里有没有 `**`：
 *
 * | 排除项 | 判据 | 对应真实误报 |
 * |---|---|---|
 * | 块注释 | 维护一个「是否在 `/* … *​/` 里」的状态（`{/*` 也算） | 大量 task 注释 |
 * | 行注释 | 去掉 `//` 之后的部分（`https://` 里的 `//` 不算：它前面是 `:`） | **`App.tsx:289`**：`? // task-120：**这句…**` |
 * | 幂运算符 | `**` 后面紧跟**空白或数字** ⇒ 是 `a ** 2` 这种运算 | **`Globe.tsx:326-327`**：`Math.sin(…) ** 2` |
 * | 字符串/模板 | `**` 前该行的未转义引号数为**奇数**（`"…"`、`'…'`、`` `…` ``） | 字符串字面量里的 `**` |
 *
 * 只要还有 `**` 剩下，就是**用户会在界面上看到的字面星号** ⇒ 失败并列出 `文件:行:片段`。
 *
 * # 它测不到什么（诚实清单）
 *
 * * **运行时拼出来**的文本（例如 `"**" + x + "**"`，或从后端/日志来的内容）—— 那是数据，不是源码文案；
 * * **非 `.tsx`** 文件（`.ts` 里的字符串若被渲染，本守卫看不到）；
 * * **属性值**里的 `**`（`title=` / `aria-label=` / `placeholder=`）—— 本轮只**列出**给 Lead 决定（见报告），
 *   没有自行改文案；
 * * `*.test.tsx` **被排除**：测试里的 JSX 不进生产产物（本文件自己的合成用例正靠这条）。
 */
import { describe, expect, it } from "vitest";

/**
 * 一行 JSX 文本里若出现 markdown 粗体记号，返回命中片段（用于报错信息）；否则 `null`。
 *
 * `inBlock` 由调用方维护（跨行的 `/* … *​/`）。
 */
export function findLiteralBold(line: string, inBlock: boolean): { hit: string | null; inBlock: boolean } {
  let rest = line;
  let block = inBlock;

  // ① 块注释状态机（`{/*` 与 `/*` 都算；同一行闭合也算）
  if (block) {
    const close = rest.indexOf("*/");
    if (close === -1) return { hit: null, inBlock: true };
    rest = rest.slice(close + 2);
    block = false;
  }
  // 行内可能有多个块注释：逐段剥掉
  for (;;) {
    const open = rest.indexOf("/*");
    if (open === -1) break;
    const close = rest.indexOf("*/", open + 2);
    if (close === -1) {
      // 从这里开始进入块注释；注释之前的部分仍要检查
      rest = rest.slice(0, open);
      block = true;
      break;
    }
    rest = rest.slice(0, open) + " " + rest.slice(close + 2);
  }

  // ② 行注释：去掉 `//` 之后的部分（`https://` 的 `//` 前面是 `:`，不算）
  const lineComment = rest.search(/(^|[^:])\/\//);
  if (lineComment !== -1) {
    const at = rest[lineComment] === "/" ? lineComment : lineComment + 1;
    rest = rest.slice(0, at);
  }

  // ③ 逐个数 `**`
  let idx = rest.indexOf("**");
  while (idx !== -1) {
    const next = rest[idx + 2] ?? "";
    const isExponent = next === "" || /\s/.test(next) || /[0-9]/.test(next);
    // 是否在字符串里：本行到该位置为止的未转义引号数为奇数
    const before = rest.slice(0, idx);
    const quotes = (before.match(/(^|[^\\])["'`]/g) ?? []).length;
    const inString = quotes % 2 === 1;
    if (!isExponent && !inString) {
      const end = rest.indexOf("**", idx + 2);
      const snippet = rest.slice(idx, end === -1 ? rest.length : end + 2).trim();
      return { hit: snippet, inBlock: block };
    }
    idx = rest.indexOf("**", idx + 2);
  }
  return { hit: null, inBlock: block };
}

describe("task-150 · JSX 文本节点里的字面 `**` 守卫", () => {
  it("扫描器本体：真实泄漏会被抓，两处已知误报天然不报", () => {
    const cases: Array<[string, string | null, string]> = [
      // —— 必须抓到：用户会看到星号的 JSX 文本
      ["          这是**真的在探测**（经本地 SOCKS 入站）", "**真的在探测**", "普通 JSX 文本"],
      ["  <strong>顶栏</strong>（App 自己画的 —— **不是** macOS 原生标题栏）", "**不是**", "元素之间的文本"],
      // —— 必须不报：两处已确认的误报形状（task-142）
      [
        '                  ? // task-120：**这句原来是「只设置系统 HTTP/SOCKS 代理」',
        null,
        "App.tsx:289 —— 三元表达式里的 // 注释（不是渲染文本）",
      ],
      ["        Math.sin(dLat / 2) ** 2 +", null, "Globe.tsx:326 —— 幂运算符"],
      ["    const x = a**2;", null, "无空格的幂运算（`**` 后面是数字）"],
      // —— 其它必须不报的形状
      ["        {/* task-142：**这里曾经写过粗体** */}", null, "JSX 注释（跨行由状态机处理）"],
      ["        /* **块注释** */", null, "块注释"],
      ["        // **注释里的粗体**", null, "行注释"],
      ['        const s = "**字符串里的**";', null, "字符串字面量"],
      ["        const t = `**模板里的**`;", null, "模板字符串"],
      ["        const u = 'https://example.com/a**b**';", null, "URL（`//` 前是 `:`，不当注释）"],
    ];
    const wrong: string[] = [];
    for (const [line, want, why] of cases) {
      const got = findLiteralBold(line, false).hit;
      if (got !== want) wrong.push(`${why}：期望 ${String(want)}，实得 ${String(got)}`);
    }
    expect(wrong, wrong.join("\n")).toEqual([]);
  });

  it("跨行块注释里的 `**` 不报（状态机）", () => {
    expect(findLiteralBold("        {/* 说明开始：**粗体**", false).hit).toBe(null);
    expect(findLiteralBold("            还有一行 **粗体**", true).hit).toBe(null);
    // 注释闭合之后的同一行仍要检查
    expect(findLiteralBold("        */} 这里是**真的**文本", true).hit).toBe("**真的**");
  });

  it("全仓 `apps/ui/src/**/*.tsx`（排除测试文件）里没有 JSX 文本节段的 `**`", async () => {
    const fs = (await import("node:" + "fs")) as {
      readFileSync: (p: string, enc: string) => string;
      readdirSync: (p: string, o: { withFileTypes: true }) => Array<{
        name: string;
        isDirectory: () => boolean;
      }>;
    };
    const path = (await import("node:" + "path")) as {
      resolve: (...parts: string[]) => string;
      join: (...parts: string[]) => string;
      relative: (from: string, to: string) => string;
    };

    const root = path.resolve("src");
    const files: string[] = [];
    const walk = (dir: string) => {
      for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
        if (e.name === "node_modules" || e.name === "dist") continue;
        const full = path.join(dir, e.name);
        if (e.isDirectory()) walk(full);
        else if (e.name.endsWith(".tsx") && !e.name.endsWith(".test.tsx")) files.push(full);
      }
    };
    walk(root);

    // 防空壳：文件数太少说明 walk/路径坏了，这时「没问题」没有意义
    expect(files.length, "扫描到的 .tsx 太少，可能是路径/遍历失效").toBeGreaterThan(15);

    const problems: string[] = [];
    for (const f of files) {
      const lines = fs.readFileSync(f, "utf8").split("\n");
      let inBlock = false;
      lines.forEach((line, i) => {
        const r = findLiteralBold(line, inBlock);
        inBlock = r.inBlock;
        if (r.hit) {
          problems.push(`${path.relative(root, f)}:${i + 1}: ${r.hit}`);
        }
      });
    }

    expect(
      problems,
      `这些 JSX 文本节点里写着 markdown 的 **粗体**，用户在界面上看到的是星号原文\n` +
        `（markdown 记号在 JSX 里不会变粗体 —— 要用 <strong>）：\n  ` +
        problems.join("\n  "),
    ).toEqual([]);
  });
});
