/**
 * 「内核与更新」一屏的**源码级**守卫（评审清单剩项）。
 *
 * # 为什么需要它们
 *
 * 本仓 UI 测试跑在 jsdom：**CSS 根本不加载**（没有样式引擎），所以
 * 「`.kv` 被定义了两次」「`.field__hint` 不折行」这类缺陷**无法用 DOM 断言抓**。
 * 范式与 `jsxTextGuard.test.tsx` / `busyKeysGuard.test.ts` / `geoTagSourceGuard.test.ts`
 * 一致：把契约写成**扫源码**的守卫，改前红、改后绿。
 *
 * # 两条 CSS 缺陷的用户可见后果
 *
 * * **`.kv` 定义两次**（`styles.css:698` 的 dl 版 `120px 1fr` 与 `:809` 的 div 版
 *   `repeat(auto-fit, minmax(140px, 1fr))`）：后者覆盖前者，于是 `Dashboard` /
 *   `Routing` / 设置页「特权助手」这些 `<dl>` 调用点拿到的是 **auto-fit** ——
 *   宽度够时 `dt,dd,dt,dd…` 会被排成一行里的多列，**键和值交错**（「Xray 核心」旁边
 *   坐着别人的值）。dl 版的 `120px 1fr` 从来没生效过。
 * * **`.field__hint` 没有 `overflow-wrap`**：核心路径、`SHA256SUMS.txt`、
 *   `~/Library/Logs/XrayTun/app-update.log` 这类长 token 会把卡片顶破（横向溢出）。
 *
 * # 为什么连「包裹 div 必须 `display: contents`」也要钉
 *
 * 调用点有两种写法：`<dl><dt><dd>`（Dashboard / Routing / 助手）与
 * `<div class="kv"><div><div class="kv__k">…</div><div class="kv__v">…</div></div></div>`
 * （更新区 / 事件报告）。单一定义要同时对两者成立：包裹 div **必须被
 * `display: contents` 拆掉**，否则它整体占一个格，三组键值会被排成 3 列而不是 3 行。
 * 这条没有 CSS 测试能覆盖（jsdom 不加载 CSS），只能扫源码。
 *
 * # 它测不到什么（诚实清单）
 *
 * * 只认 `^\.kv {` 这种**行首**写法（与本仓 CSS 格式一致）；写成 `.kv{` 会漏报，
 *   但那种写法在本文件里不存在（另有一条断言 `styles.css` 里没有 `.kv{`）。
 * * 不验颜色/字号是否真的好看（那要靠真机截图）。
 */
import { describe, expect, it } from "vitest";

async function readText(rel: string): Promise<string> {
  const fs = (await import("node:" + "fs")) as {
    readFileSync: (p: string, enc: string) => string;
  };
  const path = (await import("node:" + "path")) as {
    resolve: (...parts: string[]) => string;
  };
  // vitest 的 cwd 是 apps/ui（与 jsxTextGuard / previewFidelity 同款）。
  return fs.readFileSync(path.resolve("src", rel), "utf8");
}

/** `.kv` 的主规则出现次数（只认行首写法：本仓 CSS 就是这么写的）。 */
export function kvRuleCount(css: string): number {
  return (css.match(/^\.kv \{/gm) ?? []).length;
}

/** 取出某个类名的规则体（第一个匹配；返回 `null` = 没有这条规则）。 */
export function ruleBody(css: string, selector: string): string | null {
  const escaped = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const m = new RegExp(`^${escaped}\\s*\\{([^}]*)\\}`, "m").exec(css);
  return m ? m[1]! : null;
}

/** 更新区里出现的禁用徽章（负样本能被它抓，证明守卫不是空转）。 */
export function forbiddenBadgesIn(sectionSource: string): string[] {
  return sectionSource.match(/badge--ok/g) ?? [];
}

/** 从 `Settings.tsx` 源码里切出「核心与数据更新」这一节。 */
export function updateSectionSource(src: string): string {
  const start = src.indexOf('id="set-update"');
  const end = src.indexOf('id="set-misc"');
  expect(start, "找不到 #set-update（路径/结构变了？）").toBeGreaterThan(-1);
  expect(end, "找不到 #set-misc（切分锚点变了？）").toBeGreaterThan(start);
  return src.slice(start, end);
}

describe("2 · `.kv` 在两个维度上都是**一处**定义", () => {
  it("`styles.css` 里行首的 `.kv {` 恰好 1 次（改前 2 次：dl 版被 div 版覆盖）", async () => {
    const css = await readText("styles.css");
    expect(css.includes(".kv{"), "请用 `.kv {` 的写法，否则守卫会漏").toBe(false);
    expect(
      kvRuleCount(css),
      "`.kv` 被定义了不止一处 —— 后者会静默覆盖前者，dl 调用点的键值会交错",
    ).toBe(1);
  });

  it("那唯一一处是「键列 / 值列」两列网格，且会拆掉包裹 div（调用点外观一致）", async () => {
    const css = await readText("styles.css");

    const body = ruleBody(css, ".kv");
    expect(body, "`.kv` 必须有规则体").not.toBeNull();
    expect(body!, "键值对要用固定键列 + 1fr 值列").toMatch(
      /grid-template-columns:\s*120px\s+1fr/,
    );

    // 包裹 div 必须被拆掉（`display: contents`），否则 `<div class="kv">` 的三组键值
    // 会被排成三列，与 `<dl>` 调用点不一致。
    expect(
      /^\.kv[^{]*\{[^}]*display:\s*contents/m.test(css),
      "`.kv` 的包裹 div 没有被 display:contents 拆掉 ⇒ 两种调用点外观不一致",
    ).toBe(true);

    // 两种写法的「键」都要是暗淡色（dt 与 kv__k 同一口径）。
    const kvRules = css.match(/^\.kv[^{]*\{[^}]*\}/gm) ?? [];
    expect(
      kvRules.some((r) => /\.kv__k/.test(r) && /\.kv dt/.test(r) && /color:/.test(r)),
      "`.kv dt` 与 `.kv__k` 必须是同一条「键色」规则 ⇒ 两种调用点外观一致",
    ).toBe(true);
  });
});

describe("3 · `.field__hint` 必须能折断长串", () => {
  it("`.field__hint` 的规则体里有 `overflow-wrap`", async () => {
    const css = await readText("styles.css");
    const body = ruleBody(css, ".field__hint");
    expect(body, "找不到 `.field__hint {` 规则（选择器变了？）").not.toBeNull();
    expect(
      body!,
      "没有 overflow-wrap ⇒ 核心路径 / SHA256SUMS / 日志路径这类长 token 会把卡片顶破",
    ).toMatch(/overflow-wrap\s*:/);
  });
});

describe("5 · 更新区不许用 `.badge--ok`", () => {
  it("负样本能被抓（守卫不是空转）", () => {
    expect(forbiddenBadgesIn('<span className="badge badge--ok">已是最新</span>')).toEqual([
      "badge--ok",
    ]);
  });

  it("`Settings.tsx` 的更新区里 0 处 `badge--ok`（它的颜色实际是 `--warn`，名字与语义相反）", async () => {
    const settings = await readText("pages/Settings.tsx");
    expect(forbiddenBadgesIn(updateSectionSource(settings))).toEqual([]);
  });
});
