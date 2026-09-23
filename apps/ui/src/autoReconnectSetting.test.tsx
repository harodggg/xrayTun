/**
 * `auto_reconnect` 必须**看得见、关得掉**，而且关掉之后**不会被翻回开**（task-71）。
 *
 * # 修之前的状态
 *
 * 后端 `crates/xt-core/src/model.rs`：`#[serde(default = "yes")] pub auto_reconnect: bool`
 * —— **默认 `true`**；启动时由 `commands/core.rs` 的 `should_auto_reconnect`
 * （`was_connected && auto_reconnect && mode != Direct && !already_running`）决定要不要连回来。
 * 而全仓 UI 引用为 **0**：用户既看不到、也关不掉。
 * 用户把「退出应用」当成恢复网络的手段，而下次启动 / 登录项自启 / 自更新重启会**自动连回来**。
 *
 * # 这一组钉住什么
 *
 * 1. **可见**：它出现在「连接」分类（默认分类，不用展开/搜索就能看到），且反映后端当前值；
 * 2. **关得掉，且关得住**（本卡最有价值的一条）：勾掉 → 保存的**载荷里必须带着 `false`**
 *    → 后端回合之后读回**仍是关的**。
 *    为什么必须测载荷而不只测 UI：前端类型里**没有**声明这个字段，所以「整份回传能不能
 *    保住它」靠的是 `patch` 的 `{ ...settings, ...p }` **展开**。哪天有人把保存改成
 *    「按类型挑字段回传」，这个字段就会被静默丢掉，后端便按 `serde(default = "yes")`
 *    把它翻回 `true` —— 而界面上看不出来。这条断言就是钉这个的。
 */
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  snapshot: vi.fn(),
  tailLogs: vi.fn(),
  saveSettings: vi.fn(),
}));

vi.mock("./ipc", () => ({
  api: {
    snapshot: mocks.snapshot,
    tailLogs: mocks.tailLogs,
    saveSettings: mocks.saveSettings,
  },
  errorText: (e: unknown) =>
    typeof e === "string" ? e : e instanceof Error ? e.message : String(e),
  parseRecovery: () => null,
  subscribe: () => () => {},
}));

import Settings from "./pages/Settings";
import { scenarioSnapshot } from "./previewSnapshot";
import { StoreProvider } from "./store";

const LABEL = /启动时如果上次是连接状态/;

/**
 * 造快照：`auto_reconnect` 是**后端有、前端类型没声明**的字段，所以这里显式塞进去
 * （`value === undefined` 模拟「后端没给这个字段」）。
 */
function snap(value: boolean | undefined) {
  const base = scenarioSnapshot();
  const settings: Record<string, unknown> = { ...base.settings };
  if (value === undefined) delete settings.auto_reconnect;
  else settings.auto_reconnect = value;
  return { ...base, settings } as never;
}

async function renderSettings(snapshot: unknown = snap(true)) {
  mocks.snapshot.mockResolvedValue(snapshot);
  mocks.tailLogs.mockResolvedValue([]);
  const r = render(
    <StoreProvider>
      <Settings />
    </StoreProvider>,
  );
  const box = await screen.findByRole("checkbox", { name: LABEL });
  return { ...r, box: box as HTMLInputElement };
}

beforeEach(() => {
  vi.clearAllMocks();
  /**
   * 模拟后端 `save_settings`：它**原样持久化前端回传的整份设置**；若回传里缺这个字段，
   * 反序列化时 `#[serde(default = "yes")]` 会给出 `true` —— 这里把这条语义也模拟出来，
   * 否则「字段被丢掉」这种回归在测试里不会显形。
   */
  mocks.saveSettings.mockImplementation(async (s: Record<string, unknown>) => {
    const base = scenarioSnapshot();
    return {
      ...base,
      settings: { ...s, auto_reconnect: s.auto_reconnect ?? true },
    };
  });
});

describe("auto_reconnect：可见、可关、关得住（task-71）", () => {
  it("「连接」分类里能看到它，且反映后端当前值（默认 true ⇒ 勾选）", async () => {
    const { box } = await renderSettings(snap(true));
    expect(box.checked, "后端默认 true，界面就该显示为开").toBe(true);

    // 文案必须说清真实场景，而不是含糊的「自动连接」。
    // task-138：原来钉的是「只在三种情况下起作用」，逐行核 Rust 后**那是错的** ——
    // 正常退出不作废意图（`tray.rs:165-205` 不写 `was_connected`），
    // 所以「你自己退出 App 后再次打开」是第四种；而且运行期间的看门狗/换网重建
    // 根本不看这个开关。断言强度不变（四种都要点名 + 不许写「开机自动连接」）。
    expect(screen.getByText(/四种情形/)).toBeTruthy();
    expect(screen.getByText(/应用自更新/)).toBeTruthy();
    expect(screen.getByText(/崩溃后/)).toBeTruthy();
    expect(screen.getByText(/开机自启/)).toBeTruthy();
    expect(screen.getByText(/你自己退出 App 后再次打开/)).toBeTruthy();
    expect(screen.getByText(/管不住/)).toBeTruthy();
    // 且不许夸大成「开机自动连接」
    expect(screen.queryByText(/开机自动连接/)).toBeNull();
  });

  it("后端值为 false 时显示为关（不是写死 true）", async () => {
    const { box } = await renderSettings(snap(false));
    expect(box.checked).toBe(false);
  });

  it("**关掉并保存：载荷里必须带着 false，读回仍是 false**（不能被 serde 默认值翻回 true）", async () => {
    const { box } = await renderSettings(snap(true));
    expect(box.checked).toBe(true);

    fireEvent.click(box);
    expect(box.checked, "点了应当立刻变成未勾选").toBe(false);

    fireEvent.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() => expect(mocks.saveSettings).toHaveBeenCalledTimes(1));

    const payload = mocks.saveSettings.mock.calls[0]![0] as Record<string, unknown>;
    expect(
      payload.auto_reconnect,
      "保存载荷里没有带上 false —— 后端会按 serde 默认值 true 处理，用户等于关不掉",
    ).toBe(false);

    // 后端回合之后（store 用返回值替换快照）必须仍然是关的
    await waitFor(() => {
      expect(
        (screen.getByRole("checkbox", { name: LABEL }) as HTMLInputElement).checked,
        "保存后又被翻回开了",
      ).toBe(false);
    });
  });

  it("task-65 摘掉的那个「点了没用」的复选框**保持不在界面上**（别顺手加回来）", async () => {
    await renderSettings(snap(true));
    expect(screen.queryByText("退出时还原系统代理设置")).toBeNull();
  });
});
