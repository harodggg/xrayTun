/**
 * 「我切到香港了，出口却还是美国」——**界面的**那一半（诚实性回归）。
 *
 * # 用户原话
 *
 * > 「切换节点，没用，没有切换到香港，还是在美国」
 *
 * # 真实发生了什么（Lead 已取证）
 *
 * 他选中的是**香港**节点，但那台当时**不可用**（探测判 `egress-broken`：TCP 能连、
 * 经它的真实请求拿不到响应）。App 的回落策略**自动换到另一个可用的美国节点**
 * 继续连接 —— 而按既有约定它**不去改用户选中的项**，界面顶栏/仪表盘**又只显示
 * 「选中项」**，于是用户看到的是「我切到香港了，但出口还是美国，切换没用」。
 *
 * **行为（回落）是对的，界面是误导的。**
 *
 * # 这三条判据钉住什么（每条都能失败）
 *
 * ① **回落发生时，界面必须同时出现「实际在用」与「你选的」两个事实**
 *    —— 缺一即红（只显示一个就等于又把两件事混成一件）；
 * ② **一个 `egress-broken` 的节点在列表里必须带可见标记与失败类别**
 *    —— 不能看起来和好节点一样（否则用户会反复切到一个必然回落的节点）；
 * ③ **负向对照**：没有回落时**不许**出现「实际在用」这种与选中项重复的噪声
 *    （防狼来了）—— 这条是**天然绿**的守卫，它防的是「为了 ① 而无条件显示」。
 *
 * # 诚实清单（这些不在本文件的射程内）
 *
 * · 真机上「换节点」是否真的走了回落、回落的节点是否真的可用 —— 需要真机；
 * · CSS 不加载（jsdom）⇒ 颜色/布局只能靠 `data-*` 标记与源码守卫断言；
 * · 后端字段（`active_node` / `node_health`）由 Rust 侧提供，本文件只断言
 *   「字段到达界面后，界面说的是不是事实」。
 */
import { render, screen, within } from "@testing-library/react";
import { beforeAll, describe, expect, it, vi } from "vitest";

// 顶栏 / 仪表盘 / 节点页都从 store 取快照。这里只测它们本身，所以给一个可控的
// store 桩 —— 不跑真实 `StoreProvider`（那会走 async 轮询，与本文件无关）。
const storeMock = vi.hoisted(() => ({ value: {} as Record<string, unknown> }));
vi.mock("./store", () => ({
  StoreProvider: ({ children }: { children: unknown }) => children,
  useStore: () => storeMock.value,
}));

/** 与 `jsxTextGuard` / `topbarStatus` 同款：读源码文本做「同源」断言。 */
let readSrc: (rel: string) => string;
beforeAll(async () => {
  const fs = (await import("node:fs" as string)) as {
    readFileSync: (p: string, encoding: string) => string;
  };
  const path = (await import("node:path" as string)) as {
    resolve: (...parts: string[]) => string;
  };
  readSrc = (rel) => fs.readFileSync(path.resolve("src", rel), "utf8");
});

import App, { TopBar } from "./App";
import Dashboard from "./pages/Dashboard";
import Nodes from "./pages/Nodes";
import { scenarioSnapshot } from "./previewSnapshot";
import { formatTimestamp, type AppSnapshot } from "./types";

/** 预览快照里的两个真实节点：香港（用户选的、坏的）与美国（实际在用的）。 */
const HK = "n-hk-1";
const HK_NAME = "香港 · REALITY 01";
const US = "n-us-3";
const US_NAME = "美国 · 洛杉矶 CN2";

const NOW = Math.floor(Date.now() / 1000);

/**
 * 后端**节点尝试账**里这一条的真实形状（`apps/desktop/src/node_health.rs`
 * 的 `NodeFailureClass::{slug,label,advice}` + `last_failed_at`）。
 *
 * `detail` 刻意带上 `**`：后端原文是工程散文（`supervisor.rs`），界面必须
 * 去记号后再显示（这条也被 `failureHonesty.test.tsx` 同族守卫盯着）。
 */
const HEALTH = {
  class: "egress-broken",
  label: "节点可达但出口不通",
  advice: "换一个节点；这个节点本身能连上，但它转发不出去（墙或节点出口的问题）",
  failures: 2,
  last_failed_at: NOW - 90,
  detail: "经它发出的真实请求拿不到响应（000）。**已在接管默认路由之前中止**，系统网络未被改动。",
};

/**
 * 造快照。
 *
 * 断言用的两个后端字段（`active_node` / `node_health`）在这条修复之前**不存在**，
 * 所以这里用「先取基快照，再按字段赋值」的写法（而不是对象字面量 + 断言）：
 * 它在改前、改后都能编译，于是**改前的失败是真的断言失败**，不是编译不过。
 */
function snap(over: {
  /** 数据面实际在用的节点；`null` = 后端还没告诉我们（旧快照/未连接）。 */
  active?: string | null;
  /** 用户选中的节点（意图）。 */
  selected?: string;
  health?: Record<string, unknown> | null;
  running?: boolean;
} = {}): AppSnapshot {
  const base = scenarioSnapshot() as AppSnapshot & Record<string, unknown>;
  base.settings = { ...base.settings, selected_node: over.selected ?? HK };
  base.runtime = {
    ...base.runtime,
    running: over.running ?? true,
    routes_committed: true,
  };
  base.active_node = over.active === undefined ? US : over.active;
  // 经 `Record<string, unknown>` 赋值：改前 `node_health` 还不存在于类型里，
  // 这里也就不会变成编译错误（**改前的红必须是断言红**，不是编译红）。
  (base as unknown as Record<string, unknown>).node_health =
    over.health === undefined ? { [HK]: HEALTH } : over.health;
  return base as AppSnapshot;
}

function mount(ui: React.ReactElement, snapshot: AppSnapshot) {
  storeMock.value = {
    snapshot,
    busy: null,
    run: vi.fn(),
    probing: false,
  };
  return render(ui);
}

// ---------------------------------------------------------------- ① 两个事实

describe("① 回落发生时：界面必须**同时**给出「实际在用」与「你选的」", () => {
  it("顶栏一行里两个事实都在（缺一即红）", () => {
    mount(<TopBar view="dashboard" />, snap());
    const badge = screen.getByTestId("active-node-badge");
    const text = badge.textContent ?? "";
    expect(text, "缺「实际在用」这半 —— 用户就不知道出口其实是别人").toContain("实际在用");
    expect(text, "缺「实际在用」的节点名").toContain(US_NAME);
    expect(text, "缺「你选的」这半 —— 用户会以为自己的选择生效了").toContain("你选的");
    expect(text, "缺「你选的」的节点名").toContain(HK_NAME);
  });

  it("仪表盘状态区：与「已连接」同屏的那个节点名必须是**实际在用**的，且旁边写清「你选的」", () => {
    mount(<Dashboard onNavigate={() => {}} />, snap());
    const node = document.querySelector(".dash__state-node") as HTMLElement | null;
    expect(node, "状态区里「正在用的节点」这一处没了").toBeTruthy();
    expect(
      node!.textContent,
      "状态区写着用户选中的香港 —— 那正是「我切到香港了但出口是美国」的误导源头",
    ).toBe(US_NAME);
    expect(document.body.textContent ?? "", "缺「你选的」这半").toContain(HK_NAME);
  });

  it("回落说明**可见**（不是只藏在 title 里）：用了哪个 / 为什么换 / 你的选择没有被改动", () => {
    mount(<App />, snap());
    const notice = screen.getByTestId("fallback-notice");
    const text = notice.textContent ?? "";
    expect(text, "没说清本次用了哪个").toContain(US_NAME);
    expect(text, "没说清你选的是哪个").toContain(HK_NAME);
    expect(text, "没说清为什么换（后端账本里的类别）").toContain("节点可达但出口不通");
    expect(text, "没说清机器可读的失败类别").toContain("egress-broken");
    expect(text, "必须明说用户的选择没有被改动").toContain("你的选择没有被改动");
    // 后端原文里的 markdown 记号不许漏到界面上（与 failureHonesty 同族）。
    expect(text, "用户会看到 `**` 原文").not.toContain("**");
  });

  it("原因**拿不到**时如实说「原因见日志」，不编一个类别", () => {
    mount(<App />, snap({ health: {} }));
    const text = screen.getByTestId("fallback-notice").textContent ?? "";
    expect(text).toContain("你的选择没有被改动");
    expect(text).toContain("原因见日志");
    for (const slug of ["egress-broken", "tcp-unreachable", "local-port"]) {
      expect(text, `没有账本却写出了类别 ${slug} —— 那是编的`).not.toContain(slug);
    }
  });
});

// ---------------------------------------------------------------- ③ 负向对照

describe("③ 负向对照：没有回落时**不许**出现「实际在用」的噪声（防狼来了）", () => {
  it("实际在用的就是选中的那个 ⇒ 顶栏不出现任何「实际在用」字样", () => {
    mount(<TopBar view="dashboard" />, snap({ active: HK }));
    expect(
      document.body.textContent ?? "",
      "没有回落却冒出「实际在用」—— 与选中项重复，就是狼来了",
    ).not.toContain("实际在用");
    expect(screen.queryByTestId("active-node-badge")).toBeNull();
  });

  it("核心没在跑 ⇒ 也不许说「实际在用」（那会儿根本没人接管流量）", () => {
    mount(<TopBar view="dashboard" />, snap({ active: US, running: false }));
    expect(document.body.textContent ?? "").not.toContain("实际在用");
  });

  it("后端还没给出实际在用的节点（旧快照 / 字段缺席）⇒ 不猜，什么都不显示", () => {
    mount(<App />, snap({ active: null, health: null }));
    expect(document.body.textContent ?? "").not.toContain("实际在用");
    expect(screen.queryByTestId("fallback-notice")).toBeNull();
  });
});

// ---------------------------------------------------------------- ② 坏节点

describe("② 坏节点在列表里必须看得出来（类别 + 最近一次失败时间 + 删除入口）", () => {
  it("`egress-broken` 的节点带可见标记与失败类别，且和好节点长得不一样", () => {
    const { container } = mount(<Nodes />, snap());
    const bad = container.querySelector('[data-node-id="n-hk-1"]') as HTMLElement | null;
    expect(bad, "节点行缺少身份锚点（无法把「哪个节点坏」断言下来）").toBeTruthy();
    const text = bad!.textContent ?? "";
    expect(text, "没写类别中文名").toContain("节点可达但出口不通");
    expect(text, "没写机器可读的类别 —— 用户无法据此判断该不该修").toContain("egress-broken");
    expect(text, "没写「最近一次失败」这件事").toContain("上次失败");
    expect(text, "缺最近一次失败时间（后端字段必须有界面上的一席之地）").toContain(
      formatTimestamp(HEALTH.last_failed_at),
    );
    // jsdom 不加载 CSS ⇒ 用 data-* 标记把「不是一个好节点」钉下来（扫源码守卫之外的另一半）。
    expect(bad!.getAttribute("data-health"), "行上没有坏节点标记，看起来和好节点一样").toBe(
      "egress-broken",
    );
    // 明确的删除入口仍然在（这正是「别反复切到一个必然回落的节点」的出路）。
    expect(
      within(bad!).queryByRole("button", { name: "删除" }),
      "坏节点没有删除入口 —— 用户只能反复切到它、反复回落",
    ).toBeTruthy();
  });

  it("对照：没有失败记录的节点**不带**任何失败标记（标记不是装饰）", () => {
    const { container } = mount(<Nodes />, snap());
    const good = container.querySelector('[data-node-id="n-us-3"]') as HTMLElement | null;
    expect(good).toBeTruthy();
    expect(good!.getAttribute("data-health")).toBeNull();
    expect(good!.textContent ?? "", "好节点也被标成失败").not.toContain("上次失败");
  });

  it("坏节点正是用户选中的那个时，列表里必须写清「它这次没被使用」", () => {
    const { container } = mount(<Nodes />, snap());
    const bad = container.querySelector('[data-node-id="n-hk-1"]') as HTMLElement;
    expect(bad.textContent ?? "", "选中标记会让用户以为它正在被使用").toContain("本次回落未使用");
  });
});

// ---------------------------------------------------------------- 同源守卫

describe("同源守卫（CSS 不加载 ⇒ 用源码扫描代替布局/颜色判据）", () => {
  it("「实际在用」这句只允许在 `nodeInUse.ts` 里拼一次 —— 顶栏与仪表盘不得各写一份", () => {
    for (const rel of ["App.tsx", "pages/Dashboard.tsx", "pages/Nodes.tsx"]) {
      expect(readSrc(rel), `${rel} 里又抄了一份「实际在用」的判断`).not.toContain("实际在用");
    }
    expect(readSrc("nodeInUse.ts"), "唯一真源不见了").toContain("实际在用");
  });

  it("两个事实都来自后端字段（`active_node` / `settings.selected_node`），不从 notice 文案里猜", () => {
    const src = readSrc("nodeInUse.ts");
    expect(src, "必须读后端给的实际使用节点").toContain("active_node");
    expect(src, "必须读用户选中的节点").toContain("selected_node");
    expect(src, "不许解析 notice 文案（那是猜，不是字段）").not.toContain(".notice");
  });
});
