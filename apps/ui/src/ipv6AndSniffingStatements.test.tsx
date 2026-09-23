/**
 * task-142：A11「禁用 IPv6」这个假承诺 + A12「开 Fake-IP 就必须嗅探」。
 *
 * # 依据（全部只读核对，本次一行 Rust 未改）
 *
 * | 事实 | 依据 |
 * |---|---|
 * | `Ipv6Mode::Disabled` 与 `Passthrough` **同分支**（什么都不做） | `crates/xt-tun/src/plan.rs:233-243`；`grep -rn "Ipv6Mode::Disabled"` 全仓只命中这一处 |
 * | 核心侧只认 `Override`（不会因 `disabled` 阻断 v6） | `crates/xt-core/src/xray/config.rs:104-112` |
 * | 嗅探的真值是 `sniffing \|\| fakedns` | `crates/xt-core/src/xray/config.rs:346` |
 * | Lead 裁决 | A11 = 2b（**移除选项**，不改枚举、不改 Rust）；A12 = 选项 1（保持 `\|\|`，UI 如实说明） |
 *
 * # 这一组钉住什么
 *
 * 1. IPv6 下拉**只提供两个选项**，且**任何地方都不再出现「禁用 IPv6」**（反例）；
 * 2. 那句**必须留下的实话**在：当前版本 TUN 不阻断 IPv6，v6 仍走物理网卡；
 * 3. 已存值 `disabled` 在界面上按「不接管」显示（兼容旧设置，不静默迁移）；
 * 4. 嗅探复选框显示的是**实际生效**的值（`sniffing || fakedns`），Fake-IP 开着时
 *    **已勾选 + 不可点**，并且写明**「不生效」**与**真正能关掉的动作**（先关 Fake-IP）；
 * 5. Fake-IP 卡片里有反向说明（两个开关在两个分类里，用户会来回猜）。
 */
import { fireEvent, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  snapshot: vi.fn(),
  tailLogs: vi.fn(),
  saveSettings: vi.fn(),
  start: vi.fn(),
  stop: vi.fn(),
}));

vi.mock("./ipc", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./ipc")>();
  return {
    ...actual,
    api: {
      snapshot: mocks.snapshot,
      tailLogs: mocks.tailLogs,
      saveSettings: mocks.saveSettings,
      start: mocks.start,
      stop: mocks.stop,
    },
    subscribe: () => () => {},
  };
});

import Settings from "./pages/Settings";
import { scenarioSnapshot } from "./previewSnapshot";
import { StoreProvider } from "./store";

async function renderSettings(over: {
  ipv6?: string;
  sniffing?: boolean;
  fakedns?: boolean;
  focus: string;
}) {
  const base = scenarioSnapshot();
  mocks.snapshot.mockResolvedValue({
    ...base,
    settings: {
      ...base.settings,
      tun: { ...base.settings.tun, ipv6: over.ipv6 ?? base.settings.tun.ipv6 },
      dns: { ...base.settings.dns, sniffing: over.sniffing ?? base.settings.dns.sniffing },
      fakedns: { ...base.settings.fakedns, enabled: over.fakedns ?? base.settings.fakedns.enabled },
    },
  } as never);
  mocks.tailLogs.mockResolvedValue([]);
  render(
    <StoreProvider>
      <Settings focusSection={over.focus} />
    </StoreProvider>,
  );
  // 只渲染当前分类的分节 ⇒ 锚点要跟着 focus 走
  const anchor: Record<string, RegExp> = {
    "set-tun": /IPv6 处理/,
    "set-dns": /开启流量嗅探/,
    "set-fakeip": /启用 Fake-IP/,
  };
  await screen.findByText(anchor[over.focus]!);
  return document.body.textContent ?? "";
}

beforeEach(() => {
  vi.clearAllMocks();
  mocks.saveSettings.mockResolvedValue(scenarioSnapshot());
});

// ---------------------------------------------------------------------------
// A11
// ---------------------------------------------------------------------------

const ipv6Select = () =>
  (screen.getByText("IPv6 处理").closest(".field") as HTMLElement).querySelector(
    "select",
  ) as HTMLSelectElement;

describe("task-142 · A11：IPv6 下拉不再提供「禁用」这个假承诺", () => {
  it("只提供两个选项：不接管 / 同样接管 IPv6", async () => {
    await renderSettings({ focus: "set-tun" });
    const opts = Array.from(ipv6Select().querySelectorAll("option")).map((o) => ({
      value: o.getAttribute("value"),
      label: o.textContent,
    }));
    expect(opts.map((o) => o.value)).toEqual(["passthrough", "override"]);
    expect(opts[0]!.label).toContain("不接管");
    expect(opts[1]!.label).toContain("同样接管");
  });

  it("反例：不再有「禁用」这一档，且**任何地方**都不许出现「禁用 IPv6」这个字样", async () => {
    const text = await renderSettings({ focus: "set-tun" });
    // ① 结构上：下拉里没有任何「禁用」选项
    const labels = Array.from(ipv6Select().querySelectorAll("option")).map((o) => o.textContent ?? "");
    expect(labels.some((l) => l.includes("禁用"))).toBe(false);
    // ② 文案上：整页不再出现那个字样（Lead 的要求是「不许再出现「禁用 IPv6」这样的选项字样」）
    expect(text).not.toContain("禁用 IPv6");
  });

  it("必须留下那句实话：当前版本 TUN 不会阻断 IPv6，v6 仍走物理网卡", async () => {
    const text = await renderSettings({ focus: "set-tun" });
    expect(text).toContain("不会阻断 IPv6");
    expect(text).toContain("v6 流量仍然走物理网卡");
    expect(text, "要给出真正的做法").toContain("在系统层面关闭 IPv6");
  });

  it("兼容旧设置：已存值 `disabled` 在界面上按「不接管」显示", async () => {
    await renderSettings({ focus: "set-tun", ipv6: "disabled" });
    expect(ipv6Select().value).toBe("passthrough");
  });

  it("反例：`passthrough` / `override` 仍按各自的值显示（没有被一起改写）", async () => {
    await renderSettings({ focus: "set-tun", ipv6: "override" });
    expect(ipv6Select().value).toBe("override");
  });
});

// ---------------------------------------------------------------------------
// A12
// ---------------------------------------------------------------------------

const sniffBox = () =>
  screen.getByRole("checkbox", { name: /开启流量嗅探/ }) as HTMLInputElement;

describe("task-142 · A12：嗅探复选框显示**实际生效**的值，并说清耦合", () => {
  it("Fake-IP 开着（sniffing=false）⇒ 已勾选 + 不可点 + 写明「不生效」与真正关掉的动作", async () => {
    const text = await renderSettings({ focus: "set-dns", sniffing: false, fakedns: true });
    expect(sniffBox().checked, "显示的是生效值（sniffing || fakedns）").toBe(true);
    expect(sniffBox().disabled, "点了也没用 ⇒ 必须不可点").toBe(true);
    expect(text).toContain("强制打开");
    expect(text).toContain("不生效");
    expect(text, "要给出真正能关掉嗅探的动作").toContain("关闭 Fake-IP");
    expect(text).toContain("sniffing || fakedns");
  });

  it("`sniffing` 存的是 true 时同样是「已勾选 + 不可点 + 不生效」（判据就是 `||`）", async () => {
    const text = await renderSettings({ focus: "set-dns", sniffing: true, fakedns: true });
    expect(sniffBox().checked).toBe(true);
    expect(sniffBox().disabled).toBe(true);
    expect(text).toContain("不生效");
  });

  it("反例：Fake-IP 关着 ⇒ 复选框可点、按用户的值显示，且**不出现**「不生效」", async () => {
    const text = await renderSettings({ focus: "set-dns", sniffing: false, fakedns: false });
    expect(sniffBox().checked).toBe(false);
    expect(sniffBox().disabled).toBe(false);
    expect(text).not.toContain("不生效");
    expect(text).toContain("真的关掉了");
    // 用户点一下确实会写进草稿（可交互）
    fireEvent.click(sniffBox());
    expect(sniffBox().checked, "Fake-IP 没开时它就是一个正常开关").toBe(true);
  });

  it("反例：渲染给用户看的文本里**不许残留字面 `**`**（markdown 记号在 JSX 里不会变粗体）", async () => {
    // 这条是补 task-126/128/140 的漏：那几处我把 `**强调**` 写进了 JSX 文本，
    // 用户看到的是**真的在探测**这种带星号的原文，而当时的断言用 toContain("真的在探测")
    // 正好能从 `**真的在探测**` 里匹配到 ⇒ 测试是绿的。所以这里直接钉住「没有 **」。
    for (const focus of ["set-tun", "set-dns", "set-fakeip"]) {
      const text = await renderSettings({ focus, fakedns: true });
      expect(text, `${focus} 渲染文本里有字面 **`).not.toContain("**");
    }
  });

  it("Fake-IP 卡片有反向说明：它会强制打开嗅探，想关嗅探先关它", async () => {
    const text = await renderSettings({ focus: "set-fakeip", fakedns: true });
    expect(text).toContain("会同时把「流量嗅探」强制打开");
    expect(text).toContain("不生效");
    expect(text).toContain("先关掉这里的 Fake-IP");
  });
});
