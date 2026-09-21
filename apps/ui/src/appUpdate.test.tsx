/**
 * 「查到了 ≠ 有新版」（task-44）。
 *
 * # 背景（用户原话：「已经是最新版本的时候，不应该显示可以更新，因为会多余」）
 *
 * 后端早就算对了 `app_update_available`（`commands/snapshot.rs` 里比过版本），
 * 但前端**完全没用**它：更新按钮的门槛是 `snapshot.update.latest_app &&` ——
 * 只判断「查到了没有」。于是只要「检查客户端更新」成功过一次（无论你是不是最新版），
 * 就永远显示「更新到 vX 并重启」。
 *
 * 这与本项目修过四次的「查不到 ≠ 没有」是同一类问题的**镜像**：
 * 「查到了 ≠ 有新版」。所以这里既钉住「确实有新版才给按钮」，
 * 也钉住反面：**查不到/检查失败时不许说「已是最新」**。
 *
 * # 数据来源
 *
 * 走**真实的 store + 预览快照**（`scenarioSnapshot()`），只把 `update` 这一段换成
 * 目标场景 —— 不 mock 一个裸布尔，否则测的是测试自己。
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

/** 造一条「GitHub 上有这个版本」的条目（字段与真实 release 同形）。 */
function release(version: string): NonNullable<Update["latest_app"]> {
  return {
    size: 47_145_126,
    version,
    published_at: "2026-09-20T16:03:44Z",
    prerelease: false,
    download_url: `https://github.com/harodggg/xrayTun/releases/download/v${version}/XrayTun_${version}_x86_64_arm64.dmg`,
    digest_url: null,
  };
}

/** 用预览快照打底，只替换 `update` 这一段。 */
function snapshotWith(update: Partial<Update>): AppSnapshot {
  const base = scenarioSnapshot();
  return { ...base, update: { ...base.update, ...update } };
}

function renderSettings(update: Partial<Update>) {
  mocks.snapshot.mockResolvedValue(snapshotWith(update));
  return render(
    <StoreProvider>
      {/* task-48 之后「核心与数据更新」属于「内核与更新」这一类，默认分类不渲染它 →
          这里用**带目标的落地**进去（`focusSection`）。
          顺带说明：这条也间接证明分类是真收敛（不渲染），而不是用 CSS 藏起来。 */}
      <Settings focusSection="set-update" />
    </StoreProvider>,
  );
}

beforeEach(() => {
  vi.clearAllMocks();
});

describe("客户端更新按钮只在**确实**有新版时出现（task-44）", () => {
  it("已是最新版：不出现「更新到 … 并重启」，但要有「已是最新版本」的回执", async () => {
    // 当前版本与 GitHub 上查到的一样 → 后端会给 app_update_available=false
    renderSettings({
      latest_app: release(scenarioSnapshot().app_version),
      app_update_available: false,
      check_error: null,
    });

    expect(await screen.findByText("已是最新版本")).toBeTruthy();
    expect(screen.queryByText(/更新到/)).toBeNull();
  });

  it("确实有新版：必须出现按钮，并写清更新到哪个版本", async () => {
    renderSettings({
      latest_app: release("0.8.30"),
      app_update_available: true,
      check_error: null,
    });

    expect(await screen.findByText(/更新到 0.8.30 并重启/)).toBeTruthy();
    expect(screen.queryByText("已是最新版本")).toBeNull();
  });

  it("检查失败：说失败，不许说「已是最新」，也不给按钮", async () => {
    renderSettings({
      latest_app: null,
      app_update_available: false,
      check_error: "GitHub API 限流：60 次/小时已用完",
    });

    expect(await screen.findByText(/检查更新失败/)).toBeTruthy();
    expect(screen.getByText(/GitHub API 限流/)).toBeTruthy();
    // 「没查到」不等于「已是最新」
    expect(screen.queryByText("已是最新版本")).toBeNull();
    expect(screen.queryByText(/更新到/)).toBeNull();
  });

  it("检查失败但**残留**上次查到的 latest_app 时，仍然不许说「已是最新」", async () => {
    // 这是最容易写错的一格：只按 latest_app 判断的实现会说「已是最新」。
    renderSettings({
      latest_app: release("0.8.29"),
      app_update_available: false,
      check_error: "网络不可达",
    });

    expect(await screen.findByText(/检查更新失败/)).toBeTruthy();
    expect(screen.queryByText("已是最新版本")).toBeNull();
    expect(screen.queryByText(/更新到/)).toBeNull();
  });

  it("载荷自相矛盾（说有新版但没给版本号）时不显示按钮：给不出「更新到 X」", async () => {
    renderSettings({ latest_app: null, app_update_available: true, check_error: null });

    // 不显示按钮（否则会出现「更新到 undefined 并重启」）
    expect(screen.queryByText(/更新到/)).toBeNull();
    // 也不谎称已是最新（字段说「有新版」，只是缺了版本号）
    expect(screen.queryByText("已是最新版本")).toBeNull();
  });
});
