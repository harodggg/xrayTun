/**
 * 顶栏状态线的回归测试（task-45）。
 *
 * # 这条缺陷是什么
 *
 * 用户原话：「仪表盘的绿线 应该 跟随 软件的状态。比如 直连 系统 更换不同的颜色。」
 *
 * `styles.css` 把 `.topbar` 的底边写死成 `2px solid var(--ok)`，而全项目
 * `topbar--` **0 命中** —— 那条线从来没跟状态变过。在这套配色里绿色 = 已受保护，
 * 于是：**直连模式下它是绿的**（用户以为受保护，实际毫无代理）、**系统代理下也是绿的**
 * （实际只有读系统代理的应用走代理）、**自动兜底退回直连时还是绿的**
 * （流量已经在裸奔，界面还在报平安）。
 *
 * 这与本项目修过的「日志页把读不到说成核心没启动」「已是最新版还显示可更新」
 * 是同一族问题：**界面陈述了不实事实**。
 *
 * # 这些测试钉住什么
 *
 * 1. 五种 tone 各自的判据正确，且**判据只来自真实后端字段**；
 * 2. **硬要求：直连与系统代理不得是绿色**（绿色只属于「整机受保护」的 TUN）；
 * 3. **优先级**：故障压过模式 —— `direct_fallback` 时 `mode` 仍是 `"tun"`，
 *    若按模式取色就会画成绿色；
 * 4. **默认值是中性灰而不是绿**（快照缺席 / 漏配状态时不假装受保护）；
 * 5. CSS 里 tone → 颜色的映射正确，且每种颜色在 `--bg` 上**至少 3:1**
 *    （WCAG 1.4.11：这条线承载状态信息，属于「有意义的图形」）；
 * 6. 颜色不是唯一载体：`title` 与 `role="status"` 的可读文本必须存在且与 tone 对应。
 */
import { render } from "@testing-library/react";
import { beforeAll, describe, expect, it, vi } from "vitest";

/**
 * 直接读真实样式表的**源文本**：状态线是 CSS 决定的，只断言类名会漏掉
 * 「类名对了、颜色没跟着改」—— 而 task-45 的原始缺陷正是**颜色**写死。
 *
 * 两个绕开的坑：
 * 1. 不走 `import CSS from "./styles.css?raw"` —— `vitest.config.ts` 没开
 *    `test.css`，vitest 把 CSS 模块 stub 成空字符串（实测拿到 `""`）。
 * 2. 不用静态 `import { readFileSync } from "node:fs"` —— 本包不装
 *    `@types/node`，`tsc --noEmit` 会报 TS2307，而 `declare module` 补声明
 *    在模块文件里是 TS2664（只允许写在 `.d.ts` 里）。改用**非字面量**的动态
 *    import：TS 无法静态解析，返回 `any`，于是不需要新增 `.d.ts` 或放宽 tsconfig。
 */
let CSS = "";
beforeAll(async () => {
  const fs = (await import("node:fs" as string)) as {
    readFileSync: (p: string, encoding: string) => string;
  };
  const path = (await import("node:path" as string)) as {
    resolve: (...parts: string[]) => string;
  };
  // vitest 的工作目录是 apps/ui（`vitest.config.ts` 所在处）。
  CSS = fs.readFileSync(path.resolve("src", "styles.css"), "utf8");
});

// `TopBar` 从 store 取快照。这里只测顶栏本身，所以给一个可控的 store 桩 ——
// 不去跑真实的 StoreProvider（那会走 async 轮询，与本文件的断言无关）。
const storeMock = vi.hoisted(() => ({ value: {} as Record<string, unknown> }));
vi.mock("./store", () => ({
  StoreProvider: ({ children }: { children: unknown }) => children,
  useStore: () => storeMock.value,
}));

import { DOT_TONE_CLASS, TOPBAR_TONE_CLASS, TopBar, topbarStatus, type TopbarTone } from "./App";


/** 默认输入 = 「核心在跑、路由已接管、无错误、不在恢复流程」。各测试只改关心的字段。 */
const base = {
  mode: "tun" as const,
  running: true,
  routesCommitted: true,
  lastError: null as string | null,
  recoveryPhase: "idle" as const,
};

const s = (over: Partial<Parameters<typeof topbarStatus>[0]> = {}) => topbarStatus({ ...base, ...over });

// ---------------------------------------------------------------- 逐状态

describe("topbarStatus：五种 tone 的判据（全部来自真实字段）", () => {
  it("TUN 在跑且路由已接管 → on（**这是绿色唯一名副其实的状态**）", () => {
    const r = s({ mode: "tun", running: true, routesCommitted: true });
    expect(r.tone).toBe("on");
    expect(TOPBAR_TONE_CLASS[r.tone]).toBe("topbar--on");
    expect(r.detail).toContain("整机流量受保护");
  });

  it("系统代理在跑 → partial（**不是绿**：只有读系统代理的应用走代理）", () => {
    const r = s({ mode: "system_proxy", running: true });
    expect(r.tone).toBe("partial");
    expect(TOPBAR_TONE_CLASS[r.tone]).toBe("topbar--partial");
    expect(r.detail).toContain("只有读取系统代理的应用走代理");
  });

  it("直连模式 → off（用户主动选择的模式，中性报告，不是警告）", () => {
    const r = s({ mode: "direct", running: false });
    expect(r.tone).toBe("off");
    expect(TOPBAR_TONE_CLASS[r.tone]).toBe("topbar--off");
    expect(r.detail).toContain("不接管任何流量");
  });

  it("未连接（模式选好了但核心没跑）→ off", () => {
    const r = s({ mode: "tun", running: false });
    expect(r.tone).toBe("off");
    expect(r.detail).toContain("核心没有运行");
  });

  it("看门狗正在自动恢复 → busy（此刻既不是受保护也不是未连接）", () => {
    const r = s({ recoveryPhase: "recovering" });
    expect(r.tone).toBe("busy");
    expect(TOPBAR_TONE_CLASS[r.tone]).toBe("topbar--busy");
    expect(r.detail).toContain("正在自动恢复");
  });

  it("已退回直连（direct_fallback）→ failed（**代理承诺已破**）", () => {
    const r = s({ recoveryPhase: "failed" });
    expect(r.tone).toBe("failed");
    expect(TOPBAR_TONE_CLASS[r.tone]).toBe("topbar--failed");
    expect(r.detail).toContain("流量不再走代理");
  });

  it("核心没跑 + 上次报错 → failed（如实说故障，不装成普通的「未连接」）", () => {
    const r = s({ running: false, lastError: "端口 10808 被占用" });
    expect(r.tone).toBe("failed");
    expect(r.detail).toContain("端口 10808 被占用");
  });

  it("隧道建好但**默认路由尚未接管** → busy（进程在跑 ≠ 流量走了代理）", () => {
    const r = s({ mode: "tun", running: true, routesCommitted: false });
    expect(r.tone).toBe("busy");
    expect(r.detail).toContain("流量还没有走代理");
  });

  it("系统代理 + routes_committed=false → partial，**不是 busy**", () => {
    // `routes_committed` 是 TUN 的闸门（`supervisor.rs` 的提交路由那段在
    // `if mode == Tun` 分支内）。系统代理不接管路由，这个字段对它没有意义 ——
    // 拿它压系统代理会把「部分覆盖正常工作中」误报成「流量还没走代理」。
    const r = s({ mode: "system_proxy", running: true, routesCommitted: false });
    expect(r.tone).toBe("partial");
    expect(r.tone).not.toBe("busy");
    expect(r.tone).not.toBe("on");
  });
});

// ---------------------------------------------------------------- 反例 + 优先级

describe("反例：绿色只允许出现在「整机受保护」这一种状态", () => {
  it("直连模式**不得**是绿色（用户以为受保护，实际毫无代理）", () => {
    const r = s({ mode: "direct", running: false });
    expect(r.tone).not.toBe("on");
    expect(TOPBAR_TONE_CLASS[r.tone]).not.toBe("topbar--on");
    // 并且必须是明确的中性档，不是靠「碰巧不是绿」
    expect(r.tone).toBe("off");
  });

  it("系统代理模式**不得**是绿色（只有读系统代理的应用走代理）", () => {
    const r = s({ mode: "system_proxy", running: true, routesCommitted: true });
    expect(r.tone).not.toBe("on");
    expect(r.tone).toBe("partial");
  });

  it("所有非 on 的 tone 都不映射到 topbar--on，且 CSS 里绿只给 .topbar--on", () => {
    const others: TopbarTone[] = ["partial", "off", "busy", "failed"];
    for (const t of others) expect(TOPBAR_TONE_CLASS[t]).not.toBe("topbar--on");
    const greens: string[] = [];
    for (const m of CSS.matchAll(/\.topbar--(\w+)\s*\{[^}]*?border-bottom-color:\s*var\((--[\w-]+)\)/g)) {
      if (m[2] === "--status-on" && m[1]) greens.push(m[1]);
    }
    expect(greens).toEqual(["on"]);
  });
});

describe("优先级：故障必须压过模式（否则退回直连会被画成绿色）", () => {
  it("mode 仍是 tun + direct_fallback → failed，而不是 on", () => {
    const r = s({ mode: "tun", running: false, recoveryPhase: "failed" });
    expect(r.tone).toBe("failed");
    expect(r.tone).not.toBe("on");
  });

  it("recovering 压过「在跑 + 路由已接管」→ busy，而不是 on", () => {
    const r = s({ mode: "tun", running: true, routesCommitted: true, recoveryPhase: "recovering" });
    expect(r.tone).toBe("busy");
    expect(r.tone).not.toBe("on");
  });

  it("直连模式压过「核心在跑」→ off（直连是模式，不因进程在跑就成了受保护）", () => {
    const r = s({ mode: "direct", running: true, routesCommitted: true });
    expect(r.tone).toBe("off");
    expect(r.tone).not.toBe("on");
  });

  it("路由未接管压过「TUN + 在跑」→ busy，而不是 on", () => {
    const r = s({ mode: "tun", running: true, routesCommitted: false });
    expect(r.tone).toBe("busy");
    expect(r.tone).not.toBe("on");
  });
});

// ---------------------------------------------------------------- 默认值

describe("默认值：未知状态不得默认「已受保护」", () => {
  it("快照缺席（running=false / routesCommitted=false / 无错误）→ off", () => {
    const r = topbarStatus({
      mode: "system_proxy", // App.tsx 在无快照时的既有默认
      running: false,
      routesCommitted: false, // App.tsx 传 `?? false` —— 未知降级成「不可信」
      lastError: null,
      recoveryPhase: "idle",
    });
    expect(r.tone).toBe("off");
    expect(r.tone).not.toBe("on");
  });

  it("CSS 的 `.topbar` 兜底颜色是 --status-off，**不是 --ok**", () => {
    const block = CSS.match(/\.topbar\s*\{([\s\S]*?)\}/)?.[1] ?? "";
    // 只看声明：注释里会**提到**旧写法（`var(--ok)`）作为历史说明，那不是颜色来源。
    const decls = block.replace(/\/\*[\s\S]*?\*\//g, "");
    expect(decls).toContain("border-bottom: 2px solid var(--status-off)");
    expect(decls).not.toContain("var(--ok)");
  });
});

// ---------------------------------------------------------------- CSS 映射 + 对比度

describe("CSS：tone → 颜色，且每种颜色在 --bg 上 ≥3:1（WCAG 1.4.11）", () => {
  /**
   * 解析 `:root` 的 token。**惰性**求值 —— `CSS` 是在 `beforeAll` 里才读到的，
   * 在 `describe` 注册期算会拿到空串（这正是 vitest 的执行顺序差异）。
   */
  let cache: Record<string, string> | null = null;
  const tokens = (): Record<string, string> => {
    if (cache) return cache;
    const block = CSS.match(/:root\s*\{([\s\S]*?)\}/)?.[1] ?? "";
    const out: Record<string, string> = {};
    for (const m of block.matchAll(/(--[\w-]+)\s*:\s*([^;]+);/g)) {
      const name = m[1];
      const value = m[2];
      if (name && value) out[name] = value.trim();
    }
    cache = out;
    return out;
  };

  /** 把 `var(--x)` 或 `#rrggbb` 解析成 hex（只跟一层别名，够用）。 */
  const resolve = (token: string): string => {
    const raw = tokens()[token];
    if (!raw) throw new Error(`token ${token} 未定义`);
    const alias = raw.match(/^var\((--[\w-]+)\)$/);
    return alias ? resolve(alias[1] as string) : raw;
  };

  // 返回元组而不是 `number[]`：tsconfig 开了 `noUncheckedIndexedAccess`，
  // 解构 `number[]` 会得到 `number | undefined`，下面就没法直接算了。
  const hex2rgb = (h: string): [number, number, number] =>
    [1, 3, 5].map((i) => parseInt(h.slice(i, i + 2), 16)) as [number, number, number];
  const lin = (c: number) => {
    const v = c / 255;
    return v <= 0.04045 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4);
  };
  const lum = (h: string) => {
    const [r, g, b] = hex2rgb(h);
    return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
  };
  const contrast = (a: string, b: string) => {
    const [x, y] = [lum(a), lum(b)].sort((p, q) => q - p) as [number, number];
    return (x + 0.05) / (y + 0.05);
  };

  it("五个状态 token 都存在，且四个语义色是既有 token 的别名（不新造色）", () => {
    expect(resolve("--status-on")).toBe(resolve("--ok"));
    expect(resolve("--status-partial")).toBe(resolve("--accent"));
    expect(resolve("--status-busy")).toBe(resolve("--warn"));
    expect(resolve("--status-failed")).toBe(resolve("--danger"));
    expect(resolve("--status-off")).not.toBe(resolve("--ok"));
  });

  it("每条 .topbar--<tone> 指向对应的 --status-*", () => {
    const want: Record<string, string> = {
      on: "--status-on",
      partial: "--status-partial",
      off: "--status-off",
      busy: "--status-busy",
      failed: "--status-failed",
    };
    for (const [tone, token] of Object.entries(want)) {
      const re = new RegExp(`\\.topbar--${tone}\\s*\\{[^}]*?border-bottom-color:\\s*var\\(${token}\\)`);
      expect(CSS, `.topbar--${tone} 应映射到 ${token}`).toMatch(re);
    }
  });

  it("五种颜色 vs 页面底 --bg 全部 ≥3:1，且中性灰确实是最暗的一档", () => {
    const bg = resolve("--bg");
    const tones = ["--status-on", "--status-partial", "--status-off", "--status-busy", "--status-failed"];
    const ratios = tones.map((t) => ({ t, r: contrast(resolve(t), bg) }));
    for (const { t, r } of ratios) expect(r, `${t} 对 --bg 只有 ${r.toFixed(2)}:1`).toBeGreaterThanOrEqual(3);
    const off = ratios.find((x) => x.t === "--status-off")!;
    for (const other of ratios.filter((x) => x.t !== "--status-off")) {
      expect(off.r, "--status-off 应当比其余色调更暗（暗 = 没有覆盖）").toBeLessThan(other.r);
    }
  });

  it("dot 的 tone 映射与状态线同色（两处不得打架）", () => {
    expect(DOT_TONE_CLASS.on).toBe("dot--on");
    expect(DOT_TONE_CLASS.off).toBe("dot--off");
    expect(DOT_TONE_CLASS.failed).toBe("dot--failed");
    for (const t of ["on", "partial", "off", "busy", "failed"] as TopbarTone[]) {
      expect(CSS, `${DOT_TONE_CLASS[t]} 未在 CSS 里定义`).toContain(`.${DOT_TONE_CLASS[t]} {`);
    }
  });
});

// ---------------------------------------------------------------- 真实 DOM

describe("真实渲染：类名、title、可读文本都随状态变", () => {
  const snapshot = (over: Record<string, unknown> = {}) => ({
    settings: { mode: "tun", show_speed_in_title: false },
    runtime: {
      running: true,
      routes_committed: true,
      last_error: null,
      recovery: {
        recovering: false,
        attempt: 0,
        probe_failures: 0,
        started_unix: null,
        last_outcome: null,
        finished_unix: null,
      },
    },
    ...over,
  });

  const mount = (snap: unknown, recovery: unknown = null) => {
    storeMock.value = { snapshot: snap, busy: null, run: vi.fn(), recovery };
    return render(<TopBar view="dashboard" />);
  };

  const header = (c: HTMLElement) => c.querySelector("header.topbar") as HTMLElement;

  it("TUN 在跑 → header 带 topbar--on，且不是默认灰", () => {
    const { container } = mount(snapshot());
    const h = header(container);
    expect(h.className).toContain("topbar--on");
    expect(h.className).not.toContain("topbar--off");
  });

  it("直连模式 → header 带 topbar--off（图上不会有绿色）", () => {
    const { container } = mount(snapshot({ settings: { mode: "direct", show_speed_in_title: false } }));
    const h = header(container);
    expect(h.className).toContain("topbar--off");
    expect(h.className).not.toContain("topbar--on");
    expect(h.className).not.toContain("topbar--partial");
  });

  it("系统代理 → header 带 topbar--partial（不得是 on）", () => {
    const { container } = mount(snapshot({ settings: { mode: "system_proxy", show_speed_in_title: false } }));
    const h = header(container);
    expect(h.className).toContain("topbar--partial");
    expect(h.className).not.toContain("topbar--on");
  });

  it("退回直连 → header 带 topbar--failed，即使 mode 还是 tun", () => {
    const rec = {
      recovering: false,
      attempt: 2,
      probe_failures: 3,
      started_unix: 1,
      last_outcome: "direct_fallback",
      finished_unix: 2,
    };
    const { container } = mount(
      snapshot({ runtime: { ...snapshot().runtime, running: false, recovery: rec } }),
      rec,
    );
    const h = header(container);
    expect(h.className).toContain("topbar--failed");
    expect(h.className).not.toContain("topbar--on");
  });

  it("颜色不是唯一载体：title 与 role=status 的可读文本随状态变", () => {
    const { container, unmount } = mount(snapshot({ settings: { mode: "direct", show_speed_in_title: false } }));
    const h = header(container);
    expect(h.getAttribute("title")).toContain("不接管任何流量");
    const live = container.querySelector('[role="status"]') as HTMLElement;
    expect(live.textContent).toContain("不接管任何流量");
    unmount();

    const t = mount(snapshot());
    const h2 = header(t.container);
    expect(h2.getAttribute("title")).toContain("整机流量受保护");
    expect((t.container.querySelector('[role="status"]') as HTMLElement).textContent).toContain(
      "整机流量受保护",
    );
  });
});
