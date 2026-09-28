/**
 * 0.9 B1：设置页统一「改设置」的心智 —— **逐项自动保存 + 一次可撤销**。
 *
 * # 修之前的状态（三种范式并存 · UX C1）
 *
 * ① 设置页 = 草稿 + 显式「保存 / 放弃」（`draft !== null` 才算脏）；
 * ② 规则页 = 点单选**立即** `save_settings`；
 * ③ 顶栏模式 = 点一下立即 `set_mode`。
 * 用户从设置页学到「要保存」，于是以为规则页的切换没生效；或反过来以为改完端口就生效了。
 *
 * # 现在钉住什么（**断言用户能看到什么**）
 *
 * 1. 「保存 / 放弃」范式**不存在**了：拨一下开关就落盘（一次改动 = 一次 `save_settings`）；
 * 2. 每次保存都有回声：`role="status"` 的「已保存「X」」+ 一颗「撤销」；
 * 3. **撤销不是假的**：它把上一个值**再存一次**（`save_settings` 第二次调用的载荷 = 旧值）；
 * 4. 文本框改动**离开输入框立刻落盘**（不用等静默窗口、更不用点保存）；
 * 5. 保存失败时**不许说「已保存」**，也**不许拿旧设置去重启核心**（task-120 的不变量）；
 * 6. 「重启核心才生效」的提示语义保留（Xray 没有配置热重载）。
 */
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  snapshot: vi.fn(),
  tailLogs: vi.fn(),
  saveSettings: vi.fn(),
  start: vi.fn(),
  stop: vi.fn(),
}));

vi.mock("./ipc", () => ({
  api: {
    snapshot: mocks.snapshot,
    tailLogs: mocks.tailLogs,
    saveSettings: mocks.saveSettings,
    start: mocks.start,
    stop: mocks.stop,
  },
  errorText: (e: unknown) =>
    typeof e === "string" ? e : e instanceof Error ? e.message : String(e),
  parseRecovery: () => null,
  subscribe: () => () => {},
}));

import Settings from "./pages/Settings";
import { scenarioSnapshot } from "./previewSnapshot";
import { StoreProvider } from "./store";
import type { AppSettings } from "./types";

type Snap = ReturnType<typeof scenarioSnapshot>;

function snap(over: Partial<AppSettings> = {}): Snap {
  const base = scenarioSnapshot();
  return { ...base, settings: { ...base.settings, ...over } } as Snap;
}

/** 渲染设置页的某个分类（不传 = 默认的「连接」）。 */
async function renderSettings(focus?: string, over: Partial<AppSettings> = {}) {
  mocks.snapshot.mockResolvedValue(snap(over));
  mocks.tailLogs.mockResolvedValue([]);
  render(
    <StoreProvider>
      <Settings focusSection={focus} />
    </StoreProvider>,
  );
  await screen.findByRole("tablist");
}

/** 第 `i` 次 `save_settings` 的载荷。 */
const savePayload = (i = 0) => mocks.saveSettings.mock.calls[i]![0] as AppSettings;

const autoReconnectBox = () =>
  screen.getByRole("checkbox", { name: /自动连回来/ }) as HTMLInputElement;

beforeEach(() => {
  vi.clearAllMocks();
  /** 后端原样持久化回传的整份设置（与 `autoReconnectSetting` 的模拟同一口径）。 */
  mocks.saveSettings.mockImplementation(async (next: AppSettings) => ({
    ...snap(),
    settings: next,
  }));
  mocks.stop.mockResolvedValue(snap());
  mocks.start.mockResolvedValue(snap());
});

describe("0.9 B1 · 改完即生效 + 一次可撤销", () => {
  it("「保存 / 放弃」范式消失：拨一下开关就落盘（一次改动 = 一次 save_settings）", async () => {
    await renderSettings();

    // 旧范式必须真的不在界面上（不是被 CSS 藏起来）
    expect(screen.queryByRole("button", { name: "保存" }), "还有「保存」按钮").toBeNull();
    expect(screen.queryByRole("button", { name: "放弃" }), "还有「放弃」按钮").toBeNull();
    expect(screen.queryByText("有未保存的改动。"), "还有草稿横幅").toBeNull();

    fireEvent.click(autoReconnectBox());

    await waitFor(() => expect(mocks.saveSettings).toHaveBeenCalledTimes(1));
    expect(savePayload().auto_reconnect, "改动必须原样进入载荷").toBe(false);
  });

  it("每次保存都有回声：role=status 的「已保存「X」」+ 一颗「撤销」", async () => {
    await renderSettings();
    expect(screen.queryByRole("status"), "没改之前不该有回声").toBeNull();

    fireEvent.click(autoReconnectBox());

    const echo = await screen.findByRole("status");
    expect(echo.textContent).toContain("已保存");
    expect(echo.textContent, "回声要说清是哪一项改动").toContain("自动连回来");
    expect(screen.getByRole("button", { name: "撤销" })).toBeTruthy();
  });

  it("撤销**真的写回上一个值**（再存一次），不是只在界面上回滚一下", async () => {
    await renderSettings();
    fireEvent.click(autoReconnectBox());
    await waitFor(() => expect(mocks.saveSettings).toHaveBeenCalledTimes(1));

    fireEvent.click(screen.getByRole("button", { name: "撤销" }));

    await waitFor(() => expect(mocks.saveSettings).toHaveBeenCalledTimes(2));
    expect(savePayload(1).auto_reconnect, "撤销 = 把上一个值写回去").toBe(true);
    await waitFor(() => expect(screen.getByText(/已撤销/)).toBeTruthy());
    // 「一次可撤销」：撤销完不再挂第二颗
    expect(screen.queryByRole("button", { name: "撤销" })).toBeNull();
  });

  it("文本框改动离开输入框立刻落盘（不用等静默窗口、更不用点保存）", async () => {
    await renderSettings();
    const field = (await screen.findByText("SOCKS5 端口")).closest(".field") as HTMLElement;
    const input = field.querySelector("input") as HTMLInputElement;

    fireEvent.change(input, { target: { value: "10810" } });
    // 浏览器在焦点离开时会发 `focusout`（冒泡）；React 的 `onBlur` 就是挂在它上面的
    // （`fireEvent.blur` 只发不冒泡的 `blur`，测不出「离开输入框」这件事）。
    fireEvent.focusOut(input);

    // 200ms 远小于静默窗口（400ms）：能这么快只可能是 blur 触发的 flush
    await waitFor(() => expect(mocks.saveSettings).toHaveBeenCalledTimes(1), { timeout: 200 });
    expect(savePayload().socks_port).toBe(10810);
  });

  it("连续两次改动**都落盘**，且第二次载荷带着第一次的结果（排队，不是忙时丢弃）", async () => {
    await renderSettings();

    fireEvent.click(autoReconnectBox());
    // 第二次改动在第一次还没返回时就发生。`run()` 在这种情况会**直接丢弃**第二次
    // （`busy` 非空 ⇒ return false），而自动保存必须每次都落盘。
    fireEvent.click(screen.getByRole("checkbox", { name: /允许局域网设备使用本机代理/ }));

    await waitFor(() => expect(mocks.saveSettings).toHaveBeenCalledTimes(2));
    expect(savePayload(0).auto_reconnect).toBe(false);
    expect(savePayload(1).auto_reconnect, "第二次载荷丢了第一次的改动").toBe(false);
    expect(savePayload(1).allow_lan).toBe(true);
  });

  it("保存失败 ⇒ **不许说「已保存」**，也**不许拿旧设置去重启核心**（task-120 不变量）", async () => {
    await renderSettings("set-core");
    mocks.saveSettings.mockRejectedValue(new Error("端口 80 需要管理员权限"));

    const field = (await screen.findByText("Xray 可执行文件路径")).closest(".field") as HTMLElement;
    const input = field.querySelector("input:not([readonly])") as HTMLInputElement;
    fireEvent.change(input, { target: { value: "/tmp/xray" } });

    fireEvent.click(screen.getByRole("button", { name: "保存并重启核心" }));
    await waitFor(() => expect(mocks.saveSettings).toHaveBeenCalledTimes(1));

    expect(mocks.stop, "保存失败却重启了核心 —— 用户会以为改动生效了").not.toHaveBeenCalled();
    expect(mocks.start).not.toHaveBeenCalled();
    expect(screen.queryByText(/已保存/), "没存下去却说「已保存」").toBeNull();
  });

  it("保留「重启核心才生效」的提示语义（自动保存 ≠ 生效）", async () => {
    await renderSettings("set-core");
    const text = document.body.textContent ?? "";
    expect(text, "要说清改动是立刻保存的").toContain("立刻保存");
    expect(text, "要说清核心只在启动时读配置").toContain("启动时");
    expect(text, "要给出让改动生效的动作").toContain("重启核心");
  });
});

describe("0.9 C1/G4 · 术语人话 + 「嗅探」收进高级折叠", () => {
  it("C1：内部术语「哨兵」不再是唯一标签 —— 人话在前、技术词在括号里，且**留在明面**", async () => {
    await renderSettings("set-tun");
    const label = screen.getByText("隧道内 DNS 地址（哨兵）");
    expect(label).toBeTruthy();
    expect(label.closest("details"), "这条风险解释必须留在明面，不许折进「高级」").toBeNull();
  });

  it("G4：「流量嗅探」保留在界面上，但收进默认收起的「高级」折叠 + 一句话说明", async () => {
    await renderSettings("set-dns");
    const box = screen.getByRole("checkbox", { name: /开启流量嗅探/ }) as HTMLInputElement;
    const fold = box.closest("details") as HTMLDetailsElement | null;

    expect(fold, "嗅探开关必须还在（裁决：保留功能），但要在「高级」折叠里").not.toBeNull();
    expect(fold!.open, "高级折叠默认收起").toBe(false);
    const summary = fold!.querySelector("summary")!;
    expect(summary.textContent).toContain("高级");
    expect(summary.textContent, "收起时也要知道里面是什么").toContain("流量嗅探");
    // 「一句话说明」：关掉它不会更安全
    expect(fold!.textContent).toContain("关掉它并不会更安全");
  });
});
