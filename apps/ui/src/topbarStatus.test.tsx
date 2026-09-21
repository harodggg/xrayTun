/**
 * 应用状态语义的回归测试（task-45 建、task-47 扩）。
 *
 * # 这条缺陷发生过两次，根因相同：**同一件事在两处各写一份判断**
 *
 * 1. **task-45**：顶栏底边那条 2px 的线写死成 `var(--ok)` —— 全项目 `topbar--`
 *    0 命中，那条线从来没跟状态变过。直连 / 系统代理 / 未连接 / 恢复中 /
 *    已退回直连**全都是绿的**，而绿色在这套配色里 = 已受保护。
 * 2. **task-47**：线修好之后，`Dashboard` 的状态词**仍然不看 `mode`** ——
 *    系统代理模式下走 `!routes_committed` 分支，写出「**隧道已建立**」
 *    「默认路由尚未接管」。系统代理根本没有隧道。同一屏里**蓝线 + 「隧道已建立」**。
 *
 * # 这些测试钉住什么
 *
 * 1. 九种状态各自的 `tone / label / sub / detail` 正确，判据只来自真实后端字段；
 * 2. **系统代理与 TUN 的状态词必须不同**（task-47 的核心），且系统代理**不得**
 *    出现「隧道」字样；
 * 3. **直连不能说成「未连接」** —— 那是有意为之，不是故障；
 * 4. **反例**：绿色只允许出现在「整机受保护」这一种状态（CSS 层面也断言）；
 * 5. **优先级**：故障压过模式（`direct_fallback` 时 `mode` 仍是 `tun`）；
 * 6. **同源**：状态词的字面量只允许出现在 `topbarStatus.ts` ——
 *    `App.tsx` / `Dashboard.tsx` 里再出现一次就是把同一族缺陷种回去；
 * 7. CSS 里 tone → 颜色的映射正确，且每种颜色在 `--bg` 上 **≥3:1**
 *    （WCAG 1.4.11：这条线承载状态信息，属于「有意义的图形」）；
 * 8. 真渲染：顶栏与仪表盘在**同一份快照**下给出的状态必须一致。
 */
import { render, screen } from "@testing-library/react";
import { beforeAll, describe, expect, it, vi } from "vitest";

// `TopBar` 与 `Dashboard` 都从 store 取快照。这里只测它们本身，所以给一个可控的
// store 桩 —— 不去跑真实的 `StoreProvider`（那会走 async 轮询，与本文件无关）。
const storeMock = vi.hoisted(() => ({ value: {} as Record<string, unknown> }));
vi.mock("./store", () => ({
  StoreProvider: ({ children }: { children: unknown }) => children,
  useStore: () => storeMock.value,
}));

/**
 * 直接读源码文本做「同源」断言。两个绕开的坑：
 * 1. `import CSS from "./styles.css?raw"` 不行 —— `vitest.config.ts` 没开 `test.css`，
 *    vitest 把 CSS 模块 stub 成空串；
 * 2. 静态 `import { readFileSync } from "node:fs"` 不行 —— 本包不装 `@types/node`，
 *    `tsc --noEmit` 报 TS2307，而 `declare module` 在模块文件里是 TS2664。
 * 改用**非字面量**的动态 import：TS 无法静态解析 → 返回 `any`。
 */
let readSrc: (rel: string) => string;
beforeAll(async () => {
  const fs = (await import("node:fs" as string)) as {
    readFileSync: (p: string, encoding: string) => string;
  };
  const path = (await import("node:path" as string)) as {
    resolve: (...parts: string[]) => string;
  };
  // vitest 的工作目录是 apps/ui（`vitest.config.ts` 所在处）。
  readSrc = (rel) => fs.readFileSync(path.resolve("src", rel), "utf8");
});

import { TopBar } from "./App";
import Dashboard from "./pages/Dashboard";
import { scenarioSnapshot } from "./previewSnapshot";
import {
  appStatus,
  DASH_TONE_CLASS,
  DOT_TONE_CLASS,
  TOPBAR_TONE_CLASS,
  type StatusTone,
} from "./topbarStatus";

const CSS = () => readSrc("styles.css");

const IDLE_RECOVERY = {
  phase: "idle" as const,
  text: null,
  hint: null,
  button: "connect" as const,
  justRecovered: false,
};
const RECOVERING = {
  phase: "recovering" as const,
  text: "正在自动恢复（第 2 次）",
  hint: null,
  button: "recovering" as const,
  justRecovered: false,
};
const FAILED = {
  phase: "failed" as const,
  text: "自动恢复失败（第 2 次），已退回直连 —— 流量不再走代理",
  hint: null,
  button: "connect" as const,
  justRecovered: false,
};

/** 默认输入 = 「核心在、在跑、路由已接管、无错误、不在恢复流程」。 */
const base = {
  mode: "tun" as const,
  running: true,
  routesCommitted: true,
  lastError: null as string | null,
  corePath: "/Applications/XrayTun.app/Contents/Resources/xray",
  recovery: IDLE_RECOVERY,
  socksPort: 10808,
  httpPort: 10809,
};

const s = (over: Partial<Parameters<typeof appStatus>[0]> = {}) => appStatus({ ...base, ...over });

// ---------------------------------------------------------------- 逐状态

describe("appStatus：九种状态的 tone / label / sub / detail", () => {
  it("TUN 在跑且路由已接管 → on（**绿色唯一名副其实的状态**）", () => {
    const r = s({ mode: "tun", running: true, routesCommitted: true });
    expect(r.tone).toBe("on");
    expect(r.label).toBe("已连接");
    expect(r.sub).toBeNull();
    expect(r.detail).toContain("整机流量受保护");
    expect(TOPBAR_TONE_CLASS[r.tone]).toBe("topbar--on");
  });

  it("系统代理在跑 → partial，且**不出现「隧道」字样**", () => {
    // task-68 改了期望值：原来断言 `label === "系统代理已启用"`。
    // **不改测试意图**（它防的是「系统代理谎报隧道已建立」—— 那条断言原样保留），
    // 只改措辞：应用从不修改系统代理设置（docs/07 路线图里那一条仍未勾选），
    // 所以「已启用」是**界面比事实强**的一句。
    const r = s({ mode: "system_proxy", running: true });
    expect(r.tone).toBe("partial");
    expect(r.label).toBe("本地代理入口已就绪");
    expect(r.label).not.toContain("已启用");
    // 第一次把「需要你手动指向」说给用户，并给真实端口。
    expect(r.sub).toContain("系统代理未被本应用修改");
    expect(r.sub).toContain("127.0.0.1:10808");
    expect(r.detail).toContain("需要手动");
    // 系统代理模式**根本没有隧道、也不接管路由** —— 这句话是 task-47 的核心缺陷。
    expect(`${r.label}${r.sub}${r.detail}`).not.toContain("隧道");
    expect(TOPBAR_TONE_CLASS[r.tone]).toBe("topbar--partial");
  });

  it("系统代理：端口读不到时**一个数字都不写**（不编）", () => {
    const r = s({ mode: "system_proxy", running: true, socksPort: null, httpPort: null });
    expect(r.label).toBe("本地代理入口已就绪");
    expect(r.sub).toContain("系统代理未被本应用修改");
    expect(`${r.label}${r.sub}${r.detail}`).not.toMatch(/127\.0\.0\.1:\d/);
    expect(r.detail).toContain("本地端口");
  });

  it("直连模式 → off，**不能说成「未连接」**（那是有意为之，不是故障）", () => {
    const r = s({ mode: "direct", running: false });
    expect(r.tone).toBe("off");
    expect(r.label).toBe("直连模式");
    expect(r.label).not.toBe("未连接");
    expect(r.sub).toContain("不接管任何流量");
  });

  it("未连接（模式选好了但核心没跑）→ off", () => {
    const r = s({ mode: "tun", running: false });
    expect(r.tone).toBe("off");
    expect(r.label).toBe("未连接");
    expect(r.sub).toBe("核心没有运行");
  });

  it("看门狗正在自动恢复 → busy，文案带「恢复」二字（不退化成「未连接」）", () => {
    const r = s({ recovery: RECOVERING });
    expect(r.tone).toBe("busy");
    expect(r.label).toBe("正在自动恢复（第 2 次）");
    expect(r.label).toContain("恢复");
    expect(r.detail).toContain("重建隧道");
  });

  it("已退回直连（direct_fallback）→ failed", () => {
    const r = s({ recovery: FAILED });
    expect(r.tone).toBe("failed");
    expect(r.label).toBe("自动恢复失败");
    expect(r.detail).toContain("流量不再走代理");
  });

  it("核心没跑 + 上次报错 → failed（如实说故障，不装成普通的「未连接」）", () => {
    const r = s({ running: false, lastError: "端口 10808 被占用" });
    expect(r.tone).toBe("failed");
    expect(r.sub).toContain("端口 10808 被占用");
  });

  it("找不到核心可执行文件 → failed（连启动都做不到）", () => {
    const r = s({ corePath: null, running: false });
    expect(r.tone).toBe("failed");
    expect(r.label).toBe("未找到核心");
  });

  it("TUN 在跑但**默认路由尚未接管** → busy（进程在跑 ≠ 流量走了代理）", () => {
    const r = s({ mode: "tun", running: true, routesCommitted: false });
    expect(r.tone).toBe("busy");
    expect(r.label).toBe("隧道已建立");
    expect(r.sub).toContain("流量还没有走代理");
  });
});

// ---------------------------------------------------------------- 核心要求

describe("task-47 的核心：系统代理与 TUN 的状态词必须不同", () => {
  it("两种模式的 label / sub / detail 全都不同", () => {
    const tun = s({ mode: "tun", running: true, routesCommitted: true });
    const sys = s({ mode: "system_proxy", running: true, routesCommitted: true });
    expect(sys.label).not.toBe(tun.label);
    expect(sys.sub).not.toBe(tun.sub);
    expect(sys.detail).not.toBe(tun.detail);
    expect(tun.label).toBe("已连接");
    expect(sys.label).toBe("本地代理入口已就绪");
  });

  it("系统代理**不得**被说成「隧道已建立」，也不得是绿色那一类", () => {
    const sys = s({ mode: "system_proxy", running: true, routesCommitted: true });
    expect(sys.label).not.toBe("隧道已建立");
    expect(sys.tone).not.toBe("on");
    expect(DASH_TONE_CLASS[sys.tone]).not.toBe(DASH_TONE_CLASS.on);
  });

  it("系统代理 + routes_committed=false → partial，**不是 busy**", () => {
    // `routes_committed` 是 TUN 的闸门（`supervisor.rs` 的提交路由那段在
    // `if mode == Tun` 分支内）。系统代理不接管路由 —— 拿它压系统代理会把
    // 「部分覆盖正常工作中」误报成「隧道已建立 / 流量还没走代理」，
    // 这正是 task-47 报的那个缺陷所走的代码路径。
    const r = s({ mode: "system_proxy", running: true, routesCommitted: false });
    expect(r.tone).toBe("partial");
    expect(r.label).toBe("本地代理入口已就绪");
    expect(r.tone).not.toBe("busy");
  });
});

describe("反例：绿色只允许出现在「整机受保护」这一种状态", () => {
  it("直连**不得**是绿色，也不得是「未连接」", () => {
    const r = s({ mode: "direct", running: false });
    expect(r.tone).toBe("off");
    expect(TOPBAR_TONE_CLASS[r.tone]).not.toBe("topbar--on");
  });

  it("系统代理**不得**是绿色", () => {
    const r = s({ mode: "system_proxy", running: true });
    expect(r.tone).toBe("partial");
    expect(r.tone).not.toBe("on");
  });

  it("CSS 里只有 .topbar--on 映射到绿色 token", () => {
    const greens: string[] = [];
    for (const m of CSS().matchAll(
      /\.topbar--(\w+)\s*\{[^}]*?border-bottom-color:\s*var\((--[\w-]+)\)/g,
    )) {
      if (m[2] === "--status-on" && m[1]) greens.push(m[1]);
    }
    expect(greens).toEqual(["on"]);
  });
});

describe("优先级：故障必须压过模式", () => {
  it("mode 仍是 tun + direct_fallback → failed，而不是 on", () => {
    const r = s({ mode: "tun", running: false, recovery: FAILED });
    expect(r.tone).toBe("failed");
    expect(r.tone).not.toBe("on");
  });

  it("recovering 压过「在跑 + 路由已接管」→ busy，而不是 on", () => {
    const r = s({ mode: "tun", running: true, routesCommitted: true, recovery: RECOVERING });
    expect(r.tone).toBe("busy");
    expect(r.tone).not.toBe("on");
  });

  it("找不到核心压过一切（包括 recovering）", () => {
    const r = s({ corePath: null, recovery: RECOVERING });
    expect(r.tone).toBe("failed");
    expect(r.label).toBe("未找到核心");
  });
});

// ---------------------------------------------------------------- 同源

describe("同源：状态词只能有一个出处", () => {
  // task-68：「系统代理已启用」→「本地代理入口已就绪」（应用从不改系统代理设置）。
  const LABELS = ["已连接", "本地代理入口已就绪", "直连模式", "未连接", "隧道已建立", "未找到核心"];
  /**
   * 只看**代码**，不看注释 —— 注释里引用状态词（「以前写的是『隧道已建立』」）
   * 是正常的文档行为，不是第二份判断。块注释与行注释都剥掉。
   */
  const codeOnly = (rel: string) =>
    readSrc(rel)
      .replace(/\/\*[\s\S]*?\*\//g, "")
      .replace(/^\s*\/\/.*$/gm, "");

  /**
   * 匹配**字面量**，不匹配子串：`"直连模式下无需启动核心"`（连接按钮的 tooltip）
   * 含有「直连模式」但这**不是**状态词。所以只查带引号的字符串与 JSX 文本这两种
   * 「把它当文案写下来」的形式。
   */
  const literalForms = (label: string) => [`"${label}"`, `>${label}<`];

  it("App.tsx 里没有任何状态词字面量（它只消费 appStatus）", () => {
    const src = codeOnly("App.tsx");
    for (const l of LABELS) {
      for (const form of literalForms(l)) {
        expect(src, `App.tsx 里把「${l}」当文案写下来了`).not.toContain(form);
      }
    }
  });

  it("Dashboard.tsx 里没有任何状态词字面量（它只消费 appStatus）", () => {
    const src = codeOnly("pages/Dashboard.tsx");
    for (const l of LABELS) {
      for (const form of literalForms(l)) {
        expect(src, `Dashboard.tsx 里把「${l}」当文案写下来了`).not.toContain(form);
      }
    }
  });

  it("状态词只出现在 topbarStatus.ts 里", () => {
    const src = readSrc("topbarStatus.ts");
    for (const l of LABELS) expect(src, `topbarStatus.ts 少了「${l}」`).toContain(`"${l}"`);
  });

  it("两个页面都必须 import 共享真源，且不得自己判 routes_committed", () => {
    expect(readSrc("App.tsx")).toContain('from "./topbarStatus"');
    const dash = readSrc("pages/Dashboard.tsx");
    expect(dash).toContain('from "../topbarStatus"');
    expect(dash).toContain("appStatus(");
    // 旧实现靠这个表达式分流状态词 —— 再出现就是又抄了一份判断。
    expect(dash).not.toContain("!runtime.routes_committed");
  });

  it("三个 class map 覆盖全部 5 个 tone，且名字与 tone 一一对应", () => {
    const tones: StatusTone[] = ["on", "partial", "off", "busy", "failed"];
    for (const t of tones) {
      expect(TOPBAR_TONE_CLASS[t]).toBe(`topbar--${t}`);
      expect(DASH_TONE_CLASS[t]).toBe(`dash__state--${t}`);
      expect(DOT_TONE_CLASS[t]).toBeTruthy();
    }
  });
});

// ---------------------------------------------------------------- CSS

describe("CSS：tone → 颜色，且每种颜色在 --bg 上 ≥3:1（WCAG 1.4.11）", () => {
  /**
   * 解析 `:root` 的 token。**惰性**求值 —— `CSS()` 每次现读，避免在
   * `describe` 注册期就解析。
   */
  let cache: Record<string, string> | null = null;
  const tokens = (): Record<string, string> => {
    if (cache) return cache;
    const block = CSS().match(/:root\s*\{([\s\S]*?)\}/)?.[1] ?? "";
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

  // 返回元组而不是 `number[]`：tsconfig 开了 `noUncheckedIndexedAccess`。
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
      expect(CSS(), `.topbar--${tone} 应映射到 ${token}`).toMatch(re);
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

  it("`.topbar` 兜底颜色是 --status-off，**不是 --ok**（未知状态不得默认「已受保护」）", () => {
    const block = CSS().match(/\.topbar\s*\{([\s\S]*?)\}/)?.[1] ?? "";
    // 只看声明：注释里会**提到**旧写法作为历史说明，那不是颜色来源。
    const decls = block.replace(/\/\*[\s\S]*?\*\//g, "");
    expect(decls).toContain("border-bottom: 2px solid var(--status-off)");
    expect(decls).not.toContain("var(--ok)");
  });

  it("顶栏点与仪表盘状态词的 5 个类都在 CSS 里定义了", () => {
    for (const cls of Object.values(DOT_TONE_CLASS)) {
      expect(CSS(), `${cls} 未在 CSS 里定义`).toContain(`.${cls} {`);
    }
    for (const cls of Object.values(DASH_TONE_CLASS)) {
      expect(CSS(), `${cls} 未在 CSS 里定义`).toContain(`.${cls} .dash__state-label {`);
    }
  });
});

// ---------------------------------------------------------------- 真渲染

describe("真渲染：顶栏与仪表盘在同一份快照下必须一致", () => {
  /** 用预览的真实场景快照当 fixture（`scenarioSnapshot` 只读 `location.search`）。 */
  const snapFor = (state: string) => {
    history.replaceState({}, "", `/?state=${state}`);
    return scenarioSnapshot();
  };
  const recoveryOf = (snap: unknown) =>
    (snap as { runtime: { recovery: unknown } }).runtime.recovery;

  const mountTopBar = (snap: unknown) => {
    storeMock.value = { snapshot: snap, busy: null, run: vi.fn(), recovery: recoveryOf(snap) };
    return render(<TopBar view="dashboard" />);
  };
  const mountDashboard = (snap: unknown) => {
    storeMock.value = {
      snapshot: snap,
      busy: null,
      run: vi.fn(),
      probing: null,
      recovery: recoveryOf(snap),
    };
    return render(<Dashboard onNavigate={() => {}} />);
  };

  it("TUN 在跑（state=connected）→ 顶栏 topbar--on，状态词「已连接」", () => {
    const snap = snapFor("connected");
    const tb = mountTopBar(snap);
    expect((tb.container.querySelector("header.topbar") as HTMLElement).className).toContain("topbar--on");
    tb.unmount();

    const d = mountDashboard(snap);
    expect(screen.getByText("已连接")).toBeTruthy();
    expect(d.container.querySelector(".dot--on")).toBeTruthy();
    d.unmount();
  });

  it("系统代理（state=uncommitted）→ 顶栏 topbar--partial，状态词「本地代理入口已就绪」**且没有「隧道已建立」**", () => {
    // 这正是 task-47 报的那条路径：mode=system_proxy 且 routes_committed=false，
    // 改前 Dashboard 会走 !routes_committed 分支写出「隧道已建立」。
    const snap = snapFor("uncommitted");
    expect(snap.settings.mode).toBe("system_proxy");
    expect(snap.runtime.routes_committed).toBe(false);

    const tb = mountTopBar(snap);
    expect((tb.container.querySelector("header.topbar") as HTMLElement).className).toContain(
      "topbar--partial",
    );
    tb.unmount();

    const d = mountDashboard(snap);
    // task-68：断言的是**状态词本身**（不是「系统代理已启用」那句比事实强的话）。
    expect(screen.getByText("本地代理入口已就绪")).toBeTruthy();
    expect(screen.queryByText("系统代理已启用")).toBeNull();
    expect(screen.queryByText("隧道已建立")).toBeNull();
    expect(d.container.querySelector(".dot--partial")).toBeTruthy();
    d.unmount();
  });

  it("两条线/点同 tone：同一快照下顶栏类与状态区类指的是同一个 tone", () => {
    const snap = snapFor("uncommitted");
    const status = appStatus({
      mode: snap.settings.mode,
      running: snap.runtime.running,
      routesCommitted: snap.runtime.routes_committed,
      lastError: snap.runtime.last_error,
      corePath: snap.core.path,
      recovery: { phase: "idle", text: null, hint: null, button: "connect", justRecovered: false },
      socksPort: snap.settings.socks_port,
      httpPort: snap.settings.http_port,
    });
    const tb = mountTopBar(snap);
    const d = mountDashboard(snap);
    expect((tb.container.querySelector("header.topbar") as HTMLElement).className).toContain(
      TOPBAR_TONE_CLASS[status.tone],
    );
    expect(d.container.querySelector("." + DASH_TONE_CLASS[status.tone])).toBeTruthy();
    expect(d.container.querySelector("." + DOT_TONE_CLASS[status.tone])).toBeTruthy();
    tb.unmount();
    d.unmount();
  });
});
