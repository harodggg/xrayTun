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
 *
 * # task-194：三处**客户端结论**改用客户端专属字段（`check_error_app`）
 *
 * `check_error` 是客户端 / 核心 / geo **三路共用**的合并字段
 * （`apps/desktop/src/state.rs:281-297` 明写「仪表盘判断客户端有没有新版**不要**用它」；
 * 写入者是 `version_check.rs::apply_app_check_result` 与 `apply_core_geo_check_result`）。
 * 用它给**客户端**的结论当门有一个**功能性**后果（不只是文案）：
 *
 * > **核心/geo 检查失败** ⇒ 即使 `app_update_available && latest_app`（客户端已确证有新版），
 * > `Settings.tsx:1330` 的安装按钮**直接消失** —— 「已知有新版却无从安装」；
 * > 另外 `:1342` 的「已是最新版本」回执也被吞掉，`:1306` 的标签会说「**客户端**本次没查成」。
 *
 * 本文件原来（task-44 / task-128）的 5 条用例都用合并 `check_error` 造「客户端检查失败」，
 * 与产品行为打架 ⇒ **改绑到 `check_error_app`**，判据与仪表盘 chip（`task-193`）**完全同一套**，
 * 不另造第二套。客户端失败时真实后端**两个字段都写**（`apply_app_check_result` 的 `Err`
 * 分支，`version_check.rs:142-143`），所以凡是要模拟「客户端这次查失败」的用例都同时给两个字段。
 * **红线不变**：客户端没查到 ⇒ 不许说「已是最新」、也不许给安装按钮。
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
      check_error_app: null,
    });

    expect(await screen.findByText("已是最新版本")).toBeTruthy();
    expect(screen.queryByText(/更新到/)).toBeNull();
  });

  it("确实有新版：必须出现按钮，并写清更新到哪个版本", async () => {
    renderSettings({
      latest_app: release("0.8.30"),
      app_update_available: true,
      check_error_app: null,
    });

    expect(await screen.findByText(/更新到 0.8.30 并重启/)).toBeTruthy();
    expect(screen.queryByText("已是最新版本")).toBeNull();
  });

  it("**客户端**检查失败：说失败，不许说「已是最新」，也不给按钮", async () => {
    renderSettings({
      latest_app: null,
      app_update_available: false,
      // 真实的客户端失败会**同时**写两个字段（`version_check.rs:142-143`）
      check_error: "GitHub API 限流：60 次/小时已用完",
      check_error_app: "GitHub API 限流：60 次/小时已用完",
    });

    expect(await screen.findByText(/检查更新失败/)).toBeTruthy();
    expect(screen.getByText(/GitHub API 限流/)).toBeTruthy();
    // 「没查到」不等于「已是最新」
    expect(screen.queryByText("已是最新版本")).toBeNull();
    expect(screen.queryByText(/更新到/)).toBeNull();
  });

  it("**客户端**检查失败但**残留**上次查到的 latest_app 时，仍然不许说「已是最新」", async () => {
    // 这是最容易写错的一格：只按 latest_app 判断的实现会说「已是最新」。
    renderSettings({
      latest_app: release("0.8.29"),
      app_update_available: false,
      check_error: "网络不可达",
      check_error_app: "网络不可达",
    });

    expect(await screen.findByText(/检查更新失败/)).toBeTruthy();
    expect(screen.queryByText("已是最新版本")).toBeNull();
    expect(screen.queryByText(/更新到/)).toBeNull();
  });

  it("载荷自相矛盾（说有新版但没给版本号）时不显示按钮：给不出「更新到 X」", async () => {
    renderSettings({ latest_app: null, app_update_available: true, check_error_app: null });

    // 不显示按钮（否则会出现「更新到 undefined 并重启」）
    expect(screen.queryByText(/更新到/)).toBeNull();
    // 也不谎称已是最新（字段说「有新版」，只是缺了版本号）
    expect(screen.queryByText("已是最新版本")).toBeNull();
  });
});

// ---------------------------------------------------------------------------
// task-194 · 核心/geo 的失败**不得**影响客户端的结论
// ---------------------------------------------------------------------------

describe("task-194 · `Settings.tsx` 三处客户端结论只用 `check_error_app`", () => {
  it("**红线（功能性）**：核心检查失败 + 客户端确证有新版 ⇒ 「更新到 X 并重启」**必须还在且可用**", async () => {
    // 改前：门是 `!check_error` ⇒ 核心那条线失败时这颗按钮**直接消失**（已知有新版却无从安装）
    renderSettings({
      check_error: "GitHub 超时", // 合并字段有值 = 核心那条线失败了
      check_error_app: null, // 客户端这条线是好的
      checked_at: 1_700_000_000,
      checked_at_app: 1_700_000_000,
      latest_app: release("0.8.30"),
      app_update_available: true,
    });

    const btn = (await screen.findByRole("button", {
      name: /更新到 0\.8\.30 并重启/,
    })) as HTMLButtonElement;
    expect(btn, "客户端已确证有新版，按钮不许因为核心失败而消失").toBeTruthy();
    expect(btn.disabled, "按钮还得是可用的").toBe(false);
    // 标签如实：这次**客户端**查成了 ⇒ 不许说「本次没查成」
    expect(screen.getByText("GitHub 上的最新版")).toBeTruthy();
    expect(screen.queryByText("上次查到的最新版（本次没查成）")).toBeNull();
    // 但核心的失败仍要说出来，而且要标明是哪一类
    expect(screen.getByText(/核心检查更新失败/)).toBeTruthy();
  });

  it("**红线（回执）**：核心检查失败 + 客户端确证没有新版 ⇒ 「已是最新版本」回执**仍要出现**", async () => {
    renderSettings({
      check_error: "GitHub 超时",
      check_error_app: null,
      checked_at_app: 1_700_000_000,
      latest_app: release(scenarioSnapshot().app_version),
      app_update_available: false,
    });

    expect(await screen.findByText("已是最新版本")).toBeTruthy();
    expect(screen.queryByText(/更新到/)).toBeNull();
  });

  it("反例：**客户端**这次查失败 ⇒ 不给按钮，标签如实说「本次没查成」", async () => {
    renderSettings({
      check_error: "GitHub 403 限流",
      check_error_app: "GitHub 403 限流",
      checked_at_app: 1_700_000_000,
      latest_app: release("0.8.29"),
      app_update_available: true, // 残留值说「有新版」
    });

    expect(await screen.findByText(/客户端检查更新失败/)).toBeTruthy();
    expect(screen.getByText("上次查到的最新版（本次没查成）")).toBeTruthy();
    expect(screen.queryByRole("button", { name: /更新到 0\.8\.29 并重启/ })).toBeNull();
  });

  it("标签判据：核心失败 + 客户端成功 ⇒ **不得**说「本次没查成」；客户端失败 ⇒ 才这么说", async () => {
    // 标签断言放在**第一条**：这样「只回退标签那一处」也能被这条抓住（敏感性④）
    const coreFailed = renderSettings({
      check_error: "核心检查更新失败：GitHub 超时",
      check_error_app: null,
      latest_app: release("0.8.30"),
      app_update_available: true,
    });
    expect(await screen.findByText("GitHub 上的最新版")).toBeTruthy();
    expect(
      screen.queryByText("上次查到的最新版（本次没查成）"),
      "核心那条线失败，不许说成「客户端本次没查成」",
    ).toBeNull();
    coreFailed.unmount();

    // 客户端**自己**失败时，这句才是对的
    renderSettings({
      check_error: "网络不可达",
      check_error_app: "网络不可达",
      latest_app: release("0.8.30"),
      app_update_available: true,
    });
    expect(await screen.findByText("上次查到的最新版（本次没查成）")).toBeTruthy();
  });

  it("合并 banner 标明子系统：客户端失败说「客户端」，核心失败说「核心」", async () => {
    const first = renderSettings({
      check_error: "客户端侧错误原文",
      check_error_app: "客户端侧错误原文",
      latest_app: null,
      app_update_available: false,
    });
    expect(await screen.findByText(/客户端检查更新失败：客户端侧错误原文/)).toBeTruthy();
    first.unmount();

    renderSettings({
      check_error: "核心侧错误原文",
      check_error_app: null,
      latest_app: null,
      app_update_available: false,
    });
    expect(await screen.findByText(/核心检查更新失败：核心侧错误原文/)).toBeTruthy();
  });
});
