/**
 * 设置页的两级结构（task-48）。
 *
 * # 用户要求
 *
 * 「优化设置里的层级，每一层层级只显示对应的内容，而不是显示整个信息。」
 * 改之前：10 个分节**全部同时渲染**（平铺长页），顶部只是 4 个锚点 ——
 * 内容层并没有收敛，而且「开机自启动」曾因为埋在 top≈2997px 处根本找不到。
 *
 * # 这几条测试钉住什么
 *
 * 1. **分类表是唯一真源**：10 个分节一个不漏、不重，且每个都能解析回自己的分类；
 * 2. **选中某类 → 其他类的分节不在 DOM 里**（不是 CSS 藏起来：藏起来的东西照样
 *    在 a11y 树里、也可能被 Tab 聚焦，而且没法用 DOM 断言）；
 * 3. **带目标的落地**（跨页意图 / 深链）落在**目标分节所属的分类**，而不是默认分类 ——
 *    这是两级结构最容易制造的新问题：「把该看见的东西藏起来」；
 * 4. **不能把需要处理的状态藏进看不见的分类**：分类标签给徽标；
 * 5. **记住上次分类**（会话级），且目标优先；
 * 6. 键盘能用 ←/→ 在分类间移动，选中态可被读出（`aria-selected`）。
 */
import { fireEvent, render, screen, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({ snapshot: vi.fn() }));

vi.mock("./ipc", async () => {
  const actual = await vi.importActual<typeof import("./ipc")>("./ipc");
  return {
    ...actual,
    api: { ...actual.api, snapshot: mocks.snapshot },
    subscribe: () => () => {},
  };
});

import App from "./App";
import Settings, {
  ALL_SETTINGS_SECTIONS,
  SETTINGS_CATEGORIES,
  categoryAttention,
  categoryOfSection,
} from "./pages/Settings";
import { scenarioSnapshot } from "./previewSnapshot";
import { StoreProvider } from "./store";
import type { AppSnapshot } from "./types";

function renderSettings(props: { focusSection?: string | null } = {}) {
  mocks.snapshot.mockResolvedValue(scenarioSnapshot());
  return render(
    <StoreProvider>
      <Settings {...props} />
    </StoreProvider>,
  );
}

/** 当前 DOM 里真实渲染出来的分节 id。 */
function renderedSections(): string[] {
  return ALL_SETTINGS_SECTIONS.filter((id) => document.getElementById(id) !== null);
}

beforeEach(() => {
  vi.clearAllMocks();
  sessionStorage.clear();
  window.location.hash = "";
});

describe("设置页两级结构（task-48）", () => {
  it("分类表覆盖全部 10 个分节：不漏、不重，且每个都能解析回自己的分类", () => {
    expect(SETTINGS_CATEGORIES.length).toBe(4);
    const all = SETTINGS_CATEGORIES.flatMap((c) => c.sections.map((s) => s.id));
    expect(all.length, "应有 10 个分节").toBe(10);
    expect(new Set(all).size, "分节不得重复归属").toBe(10);
    expect(new Set(all)).toEqual(new Set(ALL_SETTINGS_SECTIONS));

    for (const c of SETTINGS_CATEGORIES) {
      for (const s of c.sections) {
        expect(categoryOfSection(s.id), `${s.id} 的解析`).toBe(c.id);
      }
    }
    // 不认识的 id 不猜（深链/意图都靠这条）
    expect(categoryOfSection("set-not-a-thing")).toBeNull();
  });

  it("默认只渲染「连接」这一类：其他分类的分节不在 DOM 里", async () => {
    renderSettings();
    await screen.findByText("代理入口");

    expect(renderedSections().sort()).toEqual(["set-entry", "set-fakeip", "set-tun"]);
    // 「开机自启动」曾埋在 2997px 处 —— 它现在属于「系统与助手」，默认**不渲染**
    expect(document.getElementById("set-autostart")).toBeNull();
    expect(document.getElementById("set-helper")).toBeNull();
  });

  it("逐个点四类：每一类只渲染自己那一类（其他都不在 DOM）", async () => {
    renderSettings();
    await screen.findByText("代理入口");

    for (const c of SETTINGS_CATEGORIES) {
      fireEvent.click(screen.getByRole("tab", { name: new RegExp(c.label) }));
      const expected = c.sections.map((s) => s.id).sort();
      expect(renderedSections().sort(), `${c.label} 这一类`).toEqual(expected);
      expect(
        screen.getByRole("tab", { name: new RegExp(c.label) }).getAttribute("aria-selected"),
        `${c.label} 的选中态`,
      ).toBe("true");
    }
  });

  it("深链 #set-helper：选中「系统与助手」并渲染 helper 分节", async () => {
    window.location.hash = "#set-helper";
    renderSettings();

    expect(await screen.findByText(/特权助手/)).toBeTruthy();
    expect(document.getElementById("set-helper")).not.toBeNull();
    expect(
      screen.getByRole("tab", { name: /系统与助手/ }).getAttribute("aria-selected"),
    ).toBe("true");
  });

  it("跨页意图 onNavigate(\"settings\", \"set-helper\")：落在目标所属分类（目标优先于记忆）", async () => {
    // 记忆里是别的分类 —— 目标必须赢，否则「去设置修 helper」会落错地方
    sessionStorage.setItem("xraytun.settings.category", "dns");
    renderSettings({ focusSection: "set-helper" });

    expect(await screen.findByText(/特权助手/)).toBeTruthy();
    expect(
      screen.getByRole("tab", { name: /系统与助手/ }).getAttribute("aria-selected"),
    ).toBe("true");
    expect(screen.getByRole("tab", { name: /DNS/ }).getAttribute("aria-selected")).toBe("false");
  });

  it("记住上次分类（会话级）：切到 DNS 后重新挂载仍在 DNS", async () => {
    const first = renderSettings();
    await screen.findByText("代理入口");
    fireEvent.click(screen.getByRole("tab", { name: /DNS/ }));
    expect(sessionStorage.getItem("xraytun.settings.category")).toBe("dns");
    first.unmount();

    renderSettings();
    expect(await screen.findByText("DNS 解析器")).toBeTruthy();
    expect(screen.getByRole("tab", { name: /DNS/ }).getAttribute("aria-selected")).toBe("true");
  });

  it("需要处理的状态不许藏起来：「系统与助手」标签给徽标（纯函数与 DOM 同判据）", async () => {
    // 预览快照的 base 就是 helper 未安装 / 不可达
    const base = scenarioSnapshot();
    expect(categoryAttention(base).system, "helper 未就绪 → 这一类有事").toBe(true);

    renderSettings();
    await screen.findByText("代理入口");
    const tab = screen.getByRole("tab", { name: /系统与助手/ });
    // 不只用颜色：徽标带可读文本
    expect(within(tab).getByRole("img", { name: /需要处理/ })).toBeTruthy();

    // 反面：一切就绪时不该有徽标（否则徽标就变成噪声，没人再看）
    const clean = {
      ...base,
      helper: { ...base.helper, socket_present: true, reachable: true, state: "ready" },
    } as unknown as AppSnapshot;
    expect(categoryAttention(clean)).toEqual({ conn: false, dns: false, core: false, system: false });
  });

  it("键盘：Tab 进导航后用 → 从「连接」移到「DNS」", async () => {
    renderSettings();
    await screen.findByText("代理入口");

    const conn = screen.getByRole("tab", { name: /连接/ });
    conn.focus();
    fireEvent.keyDown(conn, { key: "ArrowRight" });

    expect(screen.getByRole("tab", { name: /DNS/ }).getAttribute("aria-selected")).toBe("true");
    expect(document.getElementById("set-dns"), "DNS 类的内容已渲染").not.toBeNull();
  });
});

/**
 * 跨页深链：URL 锚点直接落到设置页的对应分类（task-48）。
 *
 * 上面那组测的是设置页**自己**的分辨逻辑；这一组走**整条链路**（App 解析 URL →
 * 决定落在哪一页 → 把目标传给设置页），因为「从别处带着目标进设置」是这次的
 * 新增能力，而两级结构一旦接错，症状正是「用户被扔到默认分类、想找的东西看不见」。
 */
async function renderAppAtHash(hash: string) {
  window.location.hash = hash;
  mocks.snapshot.mockResolvedValue(scenarioSnapshot());
  render(<App />);
  // 等到设置页真的挂起来（分类栏是它恒有的部分），否则还在 loading 分支上。
  await screen.findByRole("tablist");
}

describe("跨页深链：URL 锚点 → 设置页分类（task-48）", () => {
  it("`#set-helper` 直接进设置页，并选中「系统与助手」（不是默认的「连接」）", async () => {
    await renderAppAtHash("#set-helper");

    expect(
      screen.getByRole("tab", { name: /系统与助手/ }).getAttribute("aria-selected"),
      "系统与助手 应被选中",
    ).toBe("true");
    expect(document.getElementById("set-helper"), "helper 分节真的渲染了").not.toBeNull();
    // 反面：默认分类「连接」的分节不在 DOM 里 —— 证明这是**按目标**落的，不是碰巧全渲染
    expect(document.getElementById("set-entry")).toBeNull();
  });

  it("换一个分节锚点同样成立：`#set-update` → 「内核与更新」", async () => {
    await renderAppAtHash("#set-update");

    expect(screen.getByRole("tab", { name: /内核与更新/ }).getAttribute("aria-selected")).toBe(
      "true",
    );
    expect(document.getElementById("set-update")).not.toBeNull();
    expect(document.getElementById("set-entry")).toBeNull();
  });

  it("不认识的锚点不劫持首页：`#nope` 仍停在仪表盘", async () => {
    window.location.hash = "#nope";
    mocks.snapshot.mockResolvedValue(scenarioSnapshot());
    render(<App />);
    await screen.findByRole("button", { name: /仪表盘/ }); // 等首屏快照落地

    expect(screen.queryByRole("tablist"), "不该被带进设置页").toBeNull();
    expect(document.querySelector(".nav-item.is-active")?.textContent).toContain("仪表盘");
  });
});
