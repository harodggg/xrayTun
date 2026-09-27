/**
 * 核心 / geo 更新按钮的**分支覆盖**（P0）。
 *
 * # 为什么要有这条
 *
 * 全仓 `latest_core` / `latest_geo` 在此之前**只以 `null` 出现**
 * （`singleRunButton.test.tsx` / `dashboardUpdateNotice.test.tsx` / `previewSnapshot.ts`），
 * 所以 `Settings.tsx` 里那两颗按钮的**渲染分支从来没被跑过**。
 *
 * 后果（用户报的 P0）：门槛写成 `latest_core &&`（只判断「查到了没有」）⇒
 * 只要「检查更新」成功过一次，**已装核心就是最新版时**也永远显示
 * 「更新核心到 v26.9.9（预发布）」。这与 task-44 修过的客户端按钮
 * （`appUpdate.test.tsx`：查到了 ≠ 有新版）是**同一个错**，只是落在核心这条线上。
 *
 * # 判据
 *
 * 后端比过版本后给出三态 `core_update_available`：
 *   * `true`  → 确实有新版 ⇒ 给按钮；
 *   * `false` → 已是最新   ⇒ **不**给按钮，且**明说**「核心已是最新（vX）」
 *                （静默隐藏会退化成「点了没反应」——本项目最忌讳的那种）；
 *   * `null`/缺省 → **未知**（读不到已装版本）⇒ **绝不**说「已是最新」，保守地保留按钮。
 *
 * 走**真实 store + 预览快照**，只替换 `update` 这一段。
 */
import { render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({ snapshot: vi.fn() }));

vi.mock("./ipc", async () => {
  const actual = await vi.importActual<typeof import("./ipc")>("./ipc");
  return {
    ...actual,
    api: { ...actual.api, snapshot: mocks.snapshot },
    // StoreProvider 会订阅事件；真实 subscribe 会走 Tauri API，在 jsdom 里没有。
    subscribe: () => () => {},
  };
});

import Settings from "./pages/Settings";
import { scenarioSnapshot } from "./previewSnapshot";
import { StoreProvider } from "./store";
import type { AppSnapshot } from "./types";

type Update = AppSnapshot["update"];

/** 造一条「GitHub 上有这个版本」的核心/geo 条目（字段与真实 release 同形）。 */
function release(version: string, prerelease = true): NonNullable<Update["latest_core"]> {
  return {
    size: 12_345_678,
    version,
    published_at: "2026-09-20T16:03:44Z",
    prerelease,
    download_url: `https://github.com/XTLS/Xray-core/releases/download/${version}/Xray-${version}.zip`,
    digest_url: null,
  };
}

function snapshotWith(update: Partial<Update>): AppSnapshot {
  const base = scenarioSnapshot();
  return { ...base, update: { ...base.update, ...update } };
}

function renderSettings(update: Partial<Update>) {
  mocks.snapshot.mockResolvedValue(snapshotWith(update));
  return render(
    <StoreProvider>
      {/* 与 appUpdate.test.tsx 同款：更新这一节默认分类不渲染，要带目标落地进去。 */}
      <Settings focusSection="set-update" />
    </StoreProvider>,
  );
}

beforeEach(() => {
  vi.clearAllMocks();
});

describe("核心更新按钮只在**确实**有新版时出现（P0）", () => {
  it("已装核心就是最新版：不出现「更新核心到 …」，而要明说「核心已是最新（…）」", async () => {
    // 后端比过版本 → core_update_available=false（已装 26.9.9 == GitHub 上的 v26.9.9）。
    renderSettings({
      latest_core: release("v26.9.9"),
      core_update_available: false,
      check_error_core: null,
    });

    // 正面回执：不是静默隐藏（否则用户点了「检查更新」像没生效）。
    expect(await screen.findByText(/核心已是最新（v26\.9\.9）/)).toBeTruthy();
    // 反面：绝不出现那颗会白跑一趟的按钮。
    expect(screen.queryByText(/更新核心到/)).toBeNull();
  });

  it("确实有新版：必须出现按钮，并写清更新到哪个版本（含预发布标注）", async () => {
    renderSettings({
      latest_core: release("v26.9.10", true),
      core_update_available: true,
      check_error_core: null,
    });

    expect(await screen.findByText(/更新核心到 v26\.9\.10（预发布）/)).toBeTruthy();
    expect(screen.queryByText(/核心已是最新/)).toBeNull();
  });

  it("**未知**（读不到已装版本）：不许说「已是最新」，保守地保留按钮", async () => {
    // 后端用 null 表达「读不到版本 ⇒ 无法判断」，界面**不许**把未知当 false。
    renderSettings({
      latest_core: release("v26.9.10"),
      core_update_available: null,
      check_error_core: null,
    });

    expect(await screen.findByText(/更新核心到 v26\.9\.10/)).toBeTruthy();
    expect(
      screen.queryByText(/核心已是最新/),
      "读不到版本时不许谎称已是最新（unknown ≠ false）",
    ).toBeNull();
  });

  it("还没检查过（latest_core 为 null）：按钮与「已是最新」都不出现", async () => {
    renderSettings({ latest_core: null, core_update_available: null });

    expect(await screen.findByRole("button", { name: "检查更新" })).toBeTruthy();
    expect(screen.queryByText(/更新核心到/)).toBeNull();
    expect(screen.queryByText(/核心已是最新/)).toBeNull();
  });
});

describe("geo 更新按钮与 geo 编号只印一遍", () => {
  it("查到新的 geo：按钮写清更新到哪个版本", async () => {
    renderSettings({ latest_geo: release("v26.9.10"), check_error_geo: null });

    expect(await screen.findByText(/更新 geo 到 v26\.9\.10/)).toBeTruthy();
  });

  it("geo 数据那一行**原样**显示后端给的一个编号（前端不再拼第二遍）", async () => {
    renderSettings({ geo_tag: "v26.9.9" });

    // 结构：`<div><div class="kv__k">geo 数据</div><div class="kv__v">…</div></div>`
    const key = await screen.findByText("geo 数据");
    const value = key.nextElementSibling;
    expect(value?.textContent, "geo 行必须原样显示后端的 geo_tag").toBe("v26.9.9");
    expect(value?.textContent, "绝不允许出现同一个编号印两遍").not.toContain("v26.9.9 v26.9.9");
  });
});
