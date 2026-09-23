/**
 * task-140：「实时网速显示在哪」的陈述必须与真实可见位置、真实单位一致。
 *
 * # 只读核实过的事实
 *
 * | 项 | 依据 | 结论 |
 * |---|---|---|
 * | 原生标题栏文字 | `apps/desktop/tauri.conf.json` 窗口配置：`"titleBarStyle": "Overlay"`、`"hiddenTitle": true` | macOS **隐藏**原生标题文字 ⇒ 旧文案让用户去「窗口标题栏」找，找不到 |
 * | 真正可见的顶栏 | `apps/ui/src/App.tsx:221-224`（注释写明「这条顶栏才是用户真正看到的『标题栏』」）与 `:262-275`（渲染速率） | 速率在 **App 自画顶栏**上，单位来自 `types.ts` 的 `formatRate`（1024 进制 `KiB`/`MiB`、两位小数） |
 * | 菜单栏 | `apps/desktop/src/traffic.rs:126-135`（`tray.set_title`）与 `tray_title`（`format_rate_compact`） | `↓1.2M ↑34K`；**速率为 0 时整串留空** ⇒ 「空闲时不显示」为真 |
 * | 原生窗口标题 | `traffic.rs:113-124`：`window.set_title` 保留，决定「窗口」菜单与 Mission Control 里显示什么；关掉 `hiddenTitle` 时才在标题栏可见 | 不是「完全没用」，但**标题栏上仍然看不到** |
 *
 * ⇒ 旧文案两处错：位置（窗口标题栏 → 顶栏）与单位（`MB/s`/`KB/s` → `MiB/s`/`KiB/s`）。
 * 另外它用现在时描述「顶栏显示 ↓…」，而 `show_speed_in_title` 关着时顶栏根本不渲染速率
 * （`App.tsx:224`）、菜单栏也被清空（`traffic.rs:130-135`）。
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
import { formatRate } from "./types";

/** 打开这一节：`其他`（set-misc）。 */
async function renderMisc(showSpeed: boolean) {
  const base = scenarioSnapshot();
  mocks.snapshot.mockResolvedValue({
    ...base,
    settings: { ...base.settings, show_speed_in_title: showSpeed },
  } as never);
  mocks.tailLogs.mockResolvedValue([]);
  render(
    <StoreProvider>
      <Settings focusSection="set-misc" />
    </StoreProvider>,
  );
  await screen.findByRole("checkbox", { name: /显示实时网速/ });
  return document.body.textContent ?? "";
}

/** 顶栏那条示例必须与**真正的渲染器**同形（例子写错单位就是这一条要抓的）。 */
const TOP_BAR_SAMPLE = `↓ ${formatRate(1.2 * 1024 * 1024)} ↑ ${formatRate(34 * 1024)}`;

beforeEach(() => {
  vi.clearAllMocks();
  mocks.saveSettings.mockResolvedValue(scenarioSnapshot());
});

describe("task-140 · 开关开着：说明必须指向**顶栏**与菜单栏，且单位与渲染一致", () => {
  it("位置：写的是「顶栏」，并明确否掉「原生标题栏」那条死路", async () => {
    const text = await renderMisc(true);
    expect(text).toContain("现在开着");
    expect(text).toContain("顶栏");
    expect(text).toContain("不是");
    expect(text, "要明确告诉用户标题栏找不到").toContain("去标题栏找是找不到的");
    expect(text, "不许再出现被推翻的位置名").not.toContain("窗口标题栏显示");
    // 旧的开关标签写的是「在标题栏与菜单栏显示实时网速」
    expect(text).not.toContain("在标题栏与菜单栏");
  });

  it("单位：示例必须等于 `formatRate` 的真实输出（不是 MB/s、KB/s）", async () => {
    const text = await renderMisc(true);
    expect(text).toContain(TOP_BAR_SAMPLE);
    expect(TOP_BAR_SAMPLE).toContain("MiB/s");
    expect(TOP_BAR_SAMPLE).toContain("KiB/s");
    expect(text).not.toContain("↓ 1.2 MB/s");
  });

  it("菜单栏：紧凑写法与「空闲不显示」都要在", async () => {
    const text = await renderMisc(true);
    expect(text).toContain("↓1.2M ↑34K");
    expect(text).toContain("空闲（速率为 0）时不显示");
    expect(text, "原生标题仍在更新但标题栏看不到").toContain("标题栏上仍然看不到它");
  });
});

describe("task-140 · 开关关着：不许再用现在时说「顶栏显示 ↓…」", () => {
  it("关着 ⇒ 说清现在什么都不显示，并把开关打开后的样子写清", async () => {
    const text = await renderMisc(false);
    expect(text).toContain("现在关着");
    expect(text).toContain("顶栏不再带速率");
    expect(text).toContain("菜单栏里的读数也会被清空");
    expect(text, "打开后的样子仍要写清").toContain("打开之后");
    expect(text).toContain(TOP_BAR_SAMPLE);
  });

  it("反例：两种取值的呈现必须不同（不是一段静态文案）", async () => {
    const on = await renderMisc(true);
    const off = await renderMisc(false);
    expect(on).not.toBe(off);
    expect(on).toContain("现在开着");
    expect(off).toContain("现在关着");
  });

  it("开关反映后端字段（勾选态跟着 show_speed_in_title 走）", async () => {
    await renderMisc(false);
    const box = screen.getByRole("checkbox", { name: /显示实时网速/ }) as HTMLInputElement;
    expect(box.checked).toBe(false);
    fireEvent.click(box);
    expect((box as HTMLInputElement).checked).toBe(true);
  });
});
