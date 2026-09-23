/**
 * task-138：「自动连回」开关的说明必须与真实行为一致。
 *
 * # 这一组钉住的是**穷举过的**事实（只读 Rust，未改一行）
 *
 * | # | 会连回来的情形 | 触发点 | 判据 | 关掉开关能否阻止 |
 * |---|---|---|---|---|
 * | 1 | 开机自启 | `lib.rs:301` → `AutoReconnect` | `was && auto && !direct && !running` | ✅ |
 * | 2 | 应用自更新后自动重启 | 同上 | 同上 | ✅ |
 * | 3 | 崩溃后被系统重启 | 同上 | 同上 | ✅ |
 * | 4 | **用户手动退出后再次打开** | 同上（正常退出不动作废：`tray.rs:165-205` 不写 `was_connected`；只在 `core.rs:90` / `:665` 被清零） | 同上 | ✅ |
 * | 5 | 看门狗自动重建（连续多次不通） | `core.rs:1330` | `still_mine && was_connected && failures>=2`（`core.rs:716`）——**无 `auto_reconnect`** | ❌ |
 * | 6 | 换网 / 出口变化重建 | `core.rs:1725` | `still_mine && was_connected && !recovering`（`core.rs:1553`）——**无 `auto_reconnect`** | ❌ |
 *
 * 所以旧文案「只在三种情况下起作用」有两处不对：漏了第 4 种，且把开关的适用范围
 * 说宽了（第 5/6 种根本不看它）。下面按 `settings.auto_reconnect` 的真值分两句断言。
 */
import { render, screen } from "@testing-library/react";
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

async function renderSettings(autoReconnect: boolean) {
  const base = scenarioSnapshot();
  mocks.snapshot.mockResolvedValue({
    ...base,
    settings: { ...base.settings, auto_reconnect: autoReconnect },
  } as never);
  mocks.tailLogs.mockResolvedValue([]);
  render(
    <StoreProvider>
      <Settings focusSection="set-entry" />
    </StoreProvider>,
  );
  // 等这一段渲染完：开关的标签本身始终在（提示语里的关键词会被 <strong> 拆成多个文本节点）
  await screen.findByRole("checkbox", { name: /自动连回来/ });
  return document.body.textContent ?? "";
}

beforeEach(() => {
  vi.clearAllMocks();
  mocks.saveSettings.mockResolvedValue(scenarioSnapshot());
});

describe("task-138 · 「自动连回」开关的说明按真值分两句", () => {
  it("开关**开着** ⇒ 列出四种情形（含「你自己退出 App 后再次打开」）并说明管不住运行期自愈", async () => {
    const text = await renderSettings(true);
    expect(text).toContain("四种情形");
    expect(text).toContain("开机自启");
    expect(text).toContain("应用自更新");
    expect(text).toContain("崩溃后");
    expect(text, "第四种情形必须写出来").toContain("你自己退出 App 后再次打开");
    expect(text, "必须说清它管不到运行期自愈").toContain("管不住");
    expect(text).toContain("看门狗重建");
    expect(text).toContain("换网重建");
  });

  it("开关**关着** ⇒ 说清「关着也不等于不会再自动连」，并给出真正能停下来的动作", async () => {
    const text = await renderSettings(false);
    expect(text).toContain("现在关着");
    expect(text, "四种情形在关着时也不会连").toContain("都不会把隧道拉起来");
    expect(text, "必须点破「关了≠不会自动连」").toContain("不等于");
    expect(text).toContain("不看这个开关");
    expect(text, "要有可执行的下一步").toContain("断开");
  });

  it("反例：两种取值下的呈现必须不同（不是一段静态文案）", async () => {
    const on = await renderSettings(true);
    const off = await renderSettings(false);
    expect(on).not.toBe(off);
  });

  it("反例：被推翻的那句「只在三种情况下起作用」在两种取值下都不许出现", async () => {
    expect(await renderSettings(true)).not.toContain("只在三种情况下起作用");
    expect(await renderSettings(false)).not.toContain("只在三种情况下起作用");
  });
});
