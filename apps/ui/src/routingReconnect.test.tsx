/**
 * 已连接时改「分流预设」必须**说清它还没生效**，并给一条能用的重连动作（task-70）。
 *
 * # 修之前的状态（本页的静默空操作）
 *
 * `save_settings` **只持久化 + 对齐登录项，不重启核心**；而 Xray **没有配置热重载**
 * （全仓 grep 无 reload / SIGHUP / 文件监视），规则只在核心**启动时**读取
 * （`apps/desktop/src/commands/core.rs` 的 `start_core`）。
 * 于是「已连接 → 改预设」以前是：界面没有任何变化、也没有任何提示，**而规则没生效**。
 *
 * # 这一组测试钉住什么
 *
 * 1. 已连接 + 改预设 → 提示**可见**，且文案说清「还没生效」「为什么」以及有可点的动作；
 * 2. **反例**：已连接但没改预设 → 不得出现提示（防「一进页面就提示」）；
 * 3. **反例**：**未连接**时改预设 → 不得出现「需要重连」（没有隧道可重连）——
 *    task-54 在「未运行时点模式隐式启核」上踩过这个坑，不要重复；
 * 4. 「立即重连」= **先停再起**（`start_core` 对「已在运行」是空操作，只调 start 等于没点），
 *    且新配置生成后（后端 `started_at_unix` 变新）提示自动消失；
 * 5. 边界：核心的启动时间**不早于**保存时刻 → 不提示（不许编造一件不需要做的事）。
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

import Routing from "./pages/Routing";
import { scenarioSnapshot } from "./previewSnapshot";
import { StoreProvider } from "./store";

/** 核心「启动于 N 秒前」——用它把「保存之前就起来的核心」造出来。 */
const STARTED_AGO_S = 100;

type Snap = ReturnType<typeof scenarioSnapshot>;

function snap(running: boolean, startedAt: number | null, preset = "bypass_mainland"): Snap {
  const base = scenarioSnapshot();
  return {
    ...base,
    settings: { ...base.settings, routing_preset: preset } as Snap["settings"],
    runtime: {
      ...base.runtime,
      running,
      pid: running ? 4242 : null,
      started_at_unix: startedAt,
    } as Snap["runtime"],
  };
}

const startedAgo = () => Math.floor(Date.now() / 1000) - STARTED_AGO_S;

async function renderRouting(snapshot: Snap) {
  mocks.snapshot.mockResolvedValue(snapshot);
  mocks.tailLogs.mockResolvedValue([]);
  const r = render(
    <StoreProvider>
      <Routing />
    </StoreProvider>,
  );
  await screen.findByRole("radiogroup", { name: "分流预设" });
  return r;
}

/** 点某个预设（radio 的可读名字来自它所在的 label）。 */
function pickPreset(name: RegExp) {
  fireEvent.click(screen.getByRole("radio", { name }));
}

const notice = () => screen.queryByRole("status");

beforeEach(() => {
  vi.clearAllMocks();
  mocks.stop.mockResolvedValue(snap(false, null));
  /**
   * 重连成功后的快照：启动时间**必须明显晚于**保存时刻，否则这条会**偶发**失败 ——
   * 原来这里是在 `beforeEach` 里用 `Date.now()`，「保存」发生在几十毫秒之后，
   * 一旦中间跨过整秒，`started_at < savedAt` 仍成立 ⇒ 提示不会消失 ⇒ 随机红。
   * 现在直接给一个「远在未来」的启动时间，语义仍是「核心在这次保存之后启动过」。
   */
  mocks.start.mockImplementation(async () => snap(true, Math.floor(Date.now() / 1000) + 60, "global_proxy"));
});

describe("已连接时改分流预设：不许静默不生效（task-70）", () => {
  it("已连接 + 改预设 → 提示可见，且说清「还没生效 / 为什么 / 怎么办」", async () => {
    mocks.saveSettings.mockResolvedValue(snap(true, startedAgo(), "global_proxy"));
    await renderRouting(snap(true, startedAgo()));

    expect(notice(), "改之前什么都不提示 —— 这正是要修的静默空操作").toBeNull();

    pickPreset(/全局代理/);
    await waitFor(() => expect(mocks.saveSettings).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(notice()).not.toBeNull());

    const text = notice()!.textContent ?? "";
    expect(text, "必须说清保存了但没生效").toContain("没有生效");
    expect(text, "必须说清为什么（不支持热重载 / 启动时才读）").toContain("启动时");
    expect(text, "必须给出可点的动作").toContain("立即重连");
    // 上下文：告诉用户这次保存的是哪条预设（避免「提示在说哪一次改动」）
    expect(text).toContain("全局代理");
  });

  it("反例：已连接但**没改**预设 → 不得出现提示（防「一进页面就提示」）", async () => {
    await renderRouting(snap(true, startedAgo()));
    expect(notice(), "没做任何改动却提示需要重连").toBeNull();
    expect(mocks.saveSettings).not.toHaveBeenCalled();
  });

  it("反例：**未连接**时改预设 → 不得出现「需要重连」，也不得偷偷启核", async () => {
    mocks.saveSettings.mockResolvedValue(snap(false, null, "global_proxy"));
    await renderRouting(snap(false, null)); // 核心没在跑

    pickPreset(/全局代理/);
    await waitFor(() => expect(mocks.saveSettings).toHaveBeenCalledTimes(1));

    expect(notice(), "没有隧道可重连，提示「需要重连」就是编造").toBeNull();
    expect(mocks.start, "不许因为改设置就隐式启动核心（task-54 的坑）").not.toHaveBeenCalled();
    expect(mocks.stop).not.toHaveBeenCalled();
  });

  it("「立即重连」= 先停再起（只调 start 是空操作），且重连后提示自动消失", async () => {
    mocks.saveSettings.mockResolvedValue(snap(true, startedAgo(), "global_proxy"));
    await renderRouting(snap(true, startedAgo()));

    pickPreset(/全局代理/);
    await waitFor(() => expect(notice()).not.toBeNull());

    fireEvent.click(screen.getByRole("button", { name: "立即重连" }));
    await waitFor(() => expect(mocks.start).toHaveBeenCalledTimes(1));

    expect(mocks.stop, "必须真的断开重连").toHaveBeenCalledTimes(1);
    // 顺序：先 stop 再 start（start_core 对「已在运行」是空操作，顺序反了就什么都没发生）
    expect(mocks.stop.mock.invocationCallOrder[0]).toBeLessThan(
      mocks.start.mock.invocationCallOrder[0]!,
    );
    // start 返回的快照里 started_at 更晚 ⇒ 新配置已生成 ⇒ 提示必须消失
    await waitFor(() => expect(notice(), "重连成功后还留着「需要重连」").toBeNull());
  });

  it("边界：核心的启动时间**不早于**保存时刻 → 不提示（不许编造不需要做的事）", async () => {
    // 保存时后端返回的核心启动时间**晚于**保存（相当于刚重启过）⇒ 不是「保存之前起来的」。
    // 用「远在未来」而不是 `Date.now()`：后者会在跨秒时偶发变成「早于保存」而随机红。
    mocks.saveSettings.mockResolvedValue(
      snap(true, Math.floor(Date.now() / 1000) + 60, "global_proxy"),
    );
    await renderRouting(snap(true, Math.floor(Date.now() / 1000)));

    pickPreset(/全局代理/);
    await waitFor(() => expect(mocks.saveSettings).toHaveBeenCalledTimes(1));
    expect(notice()).toBeNull();
  });
});
