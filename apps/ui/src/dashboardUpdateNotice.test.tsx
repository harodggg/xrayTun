/**
 * task-189：「有新版本」要在**不打开设置页**也能看见（用户需求「自动检测最新版本」的可见性那半）。
 *
 * # 位置与理由（写在测试里，便于复核）
 *
 * 提示挂在**仪表盘状态区**（`Dashboard.tsx`，`state.sub` 之后）——仪表盘是默认落地页
 * （`App.tsx` 的 `view === "dashboard"`），状态区是用户看「现在要不要做点什么」的地方。
 * **刻意不走 `collectNotices`**：那套按严重度只显示最急的一条，其余折进「还有 N 条提示」，
 * 新版本提示会被压住看不见 —— 那就等于没做。
 *
 * # 判据：**只用客户端专属字段**（task-193；task-189 建了这条提示、task-190 补了组合态）
 *
 * | 条件 | 结论 |
 * |---|---|
 * | `check_error_app` + `app_update_available` + `latest_app` | 「有新版本 vX.Y.Z」+「最近一次**客户端**更新检查没成功」**同时**说 |
 * | `check_error_app`（其余） | 「**客户端**更新检查没成功 ⇒ 不知道有没有新版本」+ 原因 |
 * | `app_update_available === true` | 「有新版本 vX.Y.Z」 |
 * | `latest_app` 有值 + 不可更新 | 「**客户端**已是最新（vX.Y.Z）」 |
 * | `checked_at_app === null` | 「还没检查过客户端更新」（**不假装**知道结果） |
 * | 查过但 `latest_app === null` | 「客户端检查没拿到版本信息」（**不等于**已是最新） |
 *
 * # task-193 为什么把判据从 `check_error` 改绑到 `check_error_app`
 *
 * `check_error` / `checked_at` 是**客户端 / 核心 / geo 三类检查共用**的合并字段
 * （`apps/desktop/src/state.rs:281-297` 明写「仪表盘判断客户端有没有新版**不要**用它」；
 * 写入者是 `version_check.rs::apply_app_check_result` 与 `apply_core_geo_check_result`）。
 * 用它下客户端结论有一个**真实可达**的错报：
 *
 * > **核心/geo 检查失败**（`check_error` 有值）、而**客户端检查成功且已是最新**
 * > ⇒ 旧判据会说「更新检查没成功 —— 所以不知道有没有新版本」，可事实是客户端**已确证是最新**。
 *
 * 这正是 `task-190` 修掉的「对已知事实装瞎」，只是方向相反（under-claim）。红线**不变**：
 * 客户端自己没查到（`check_error_app` 非空）时**绝不许**说成「已是最新」。
 *
 * # task-190 的修订仍然有效，只是换了字段
 *
 * 本文件原来（task-189）断言「`check_error` 非空 ⇒ 一律 failed」；task-190 改成
 * 「失败 + 已知有新版 ⇒ available + 限定语」。**那条口径不改**，只把判据字段换成
 * `check_error_app`：客户端检查失败时后端不清 `latest_app`
 * （`version_check.rs::apply_app_check_result` 的 `Err` 分支只写错误，`Ok` 分支才写 `latest_app`）
 * ⇒「查到过有新版 + 这次客户端复查失败」真实可达，红线 = 不许说成「已是最新」+ 必须带限定语。
 *
 * ⚠️ **`check_error`（合并）在本文件里只以「不该被读的字段」出现**：有一条对五个状态
 * 逐一对比的用例，钉住「加不加合并错误，结论逐字相同」。
 */
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

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

import Dashboard, { updateNotice } from "./pages/Dashboard";
import { scenarioSnapshot } from "./previewSnapshot";
import { StoreProvider } from "./store";
import type { UpdateStatus } from "./types";

const APP = {
  size: 12_345_678,
  version: "0.8.99",
  published_at: "2026-01-01T00:00:00Z",
  prerelease: false,
  download_url: "https://example.com/x.zip",
  digest_url: null,
};

const update = (over: Partial<UpdateStatus>): UpdateStatus =>
  ({
    core_version: null,
    core_managed: false,
    core_managed_version: null,
    geo_tag: null,
    geo_installed_at: null,
    latest_core: null,
    latest_geo: null,
    latest_app: null,
    app_update_available: false,
    progress: null,
    checked_at: null,
    check_error: null,
    checked_at_app: null,
    check_error_app: null,
    ...over,
  }) as UpdateStatus;

// ---------------------------------------------------------------------------
// 纯函数：五态
// ---------------------------------------------------------------------------

describe("task-189/190/193 · `updateNotice` 的判据（只用客户端专属字段）", () => {
  it("`app_update_available` ⇒ 才是「有新版本」（只认这个字段）", () => {
    expect(
      updateNotice(update({ app_update_available: true, latest_app: APP, checked_at_app: 1 })),
    ).toEqual({ kind: "available", version: "0.8.99" });
  });

  it("**有 `latest_app` 但不可更新** ⇒ 「已是最新」，不许说成「有新版本」", () => {
    expect(
      updateNotice(update({ latest_app: APP, app_update_available: false, checked_at_app: 1 })),
    ).toEqual({ kind: "latest", version: "0.8.99" });
  });

  it("**红线反例（task-193）**：`check_error`（核心/geo 失败）+ 客户端已是最新 ⇒ **必须**说已是最新，不得说「不知道」", () => {
    const n = updateNotice(
      update({
        // 合并字段有值（核心/geo 那条线失败了），但**客户端**这条线是好的、且已确证
        check_error: "核心更新检查失败：GitHub 超时",
        check_error_app: null,
        checked_at: 1,
        checked_at_app: 1,
        latest_app: APP,
        app_update_available: false,
      }),
    );
    expect(n.kind, "客户端已确证是最新，不许因为核心/geo 失败就对已知事实装瞎").toBe("latest");
    expect(n.kind === "latest" && n.version).toBe("0.8.99");
  });

  it("**合并字段 `check_error` 不得改变客户端结论**：五个状态逐一对比，加/不加合并错误结果逐字相同", () => {
    const states: Array<Partial<UpdateStatus>> = [
      { app_update_available: true, latest_app: APP, checked_at_app: 1 },
      { latest_app: APP, app_update_available: false, checked_at_app: 1 },
      { checked_at_app: null },
      { checked_at_app: 1, latest_app: null },
      { check_error_app: "客户端离线", checked_at_app: 1 },
    ];
    for (const st of states) {
      const merged = "核心/geo 更新检查失败：GitHub 超时";
      expect(
        updateNotice(update({ ...st, check_error: merged })),
        `状态 ${JSON.stringify(st)}：合并字段不该影响客户端结论`,
      ).toEqual(updateNotice(update({ ...st, check_error: null })));
    }
  });

  it("`check_error_app` + `app_update_available === false` ⇒ failed（**不许**改口说「已是最新」）", () => {
    const n = updateNotice(
      update({
        check_error_app: "GitHub API 限流（60 次/小时）",
        checked_at_app: 1,
        latest_app: APP,
      }),
    );
    expect(n.kind).toBe("failed");
    expect(n.kind === "failed" && n.reason).toContain("限流");
  });

  // ⚠️ task-190 改绑、task-193 换字段：原文是「`check_error` + `app_update_available` ⇒ failed」，
  // 现在是「`check_error_app` + 已知有新版 ⇒ available + 限定语」。红线**改绑而非删除**：
  // 这个组合态绝不许说成「已是最新」，且必须带说明来源的限定语。
  it("**组合态**：客户端复查失败 + 已知有新版 ⇒ available 且**必须**带 staleReason，绝不可是 latest", () => {
    const n = updateNotice(
      update({
        check_error_app: "网络不可达",
        app_update_available: true,
        latest_app: APP,
        checked_at_app: 1,
      }),
    );
    expect(n.kind, "曾经成功查到过有新版，不能判成「什么都不知道」").toBe("available");
    expect(n.kind === "available" && n.staleReason, "限定语不许吞掉").toBe("网络不可达");
    expect(n.kind, "红线：绝不许说成「已是最新」").not.toBe("latest");
  });

  it("反例一：`check_error_app` + **没有**已知新版 ⇒ 必须是 failed，且不是 available", () => {
    const n = updateNotice(update({ check_error_app: "离线", latest_app: null, checked_at_app: 1 }));
    expect(n.kind).toBe("failed");
  });

  it("`checked_at_app === null` ⇒ unknown（**即使 `checked_at` 有值**：核心/geo 查过 ≠ 客户端查过）", () => {
    expect(updateNotice(update({ checked_at: 1_700_000_000, checked_at_app: null })).kind).toBe(
      "unknown",
    );
  });

  it("查过但没拿到版本信息 ⇒ 也是 unknown（不等于已是最新）", () => {
    expect(updateNotice(update({ checked_at_app: 1, latest_app: null })).kind).toBe("unknown");
  });
});

// ---------------------------------------------------------------------------
// 渲染 + 跳转
// ---------------------------------------------------------------------------

async function renderDash(u: UpdateStatus) {
  const base = scenarioSnapshot();
  mocks.snapshot.mockResolvedValue({ ...base, update: u } as never);
  mocks.tailLogs.mockResolvedValue([]);
  const onNavigate = vi.fn();
  render(
    <StoreProvider>
      <Dashboard onNavigate={onNavigate} />
    </StoreProvider>,
  );
  await screen.findByText("环境自检与诊断");
  return onNavigate;
}

describe("task-189 · 仪表盘上的那条提示", () => {
  it("有新版 ⇒ 「有新版本 v0.8.99」，可点，落到 `settings#set-update`", async () => {
    const onNavigate = await renderDash(
      update({ app_update_available: true, latest_app: APP, checked_at_app: 1_700_000_000 }),
    );
    const chip = screen.getByRole("button", { name: /有新版本 v0\.8\.99/ });
    fireEvent.click(chip);
    expect(onNavigate).toHaveBeenCalledWith("settings", "set-update");
  });

  it("**客户端没查到** ⇒ 文案必须看得出是「没查到」，且**不得**出现「已是最新」", async () => {
    await renderDash(
      update({ check_error_app: "GitHub API 限流（60 次/小时）", checked_at_app: 1 }),
    );
    // 文案点名「客户端」：失败原因来自 `check_error_app`，与核心/geo 无关（task-193）
    const chip = screen.getByRole("button", { name: /客户端更新检查没成功/ });
    expect(chip.textContent).toContain("不知道有没有新版本");
    expect(chip.getAttribute("title")).toContain("限流");
    expect(screen.queryByText(/已是最新/), "红线：没查到 ≠ 已是最新").toBeNull();
    // 锚定「有新版本 v…」：否则会命中「不知道有**没有新版本**」里的子串（我自己踩到过）
    expect(screen.queryByText(/有新版本 v/)).toBeNull();
  });

  it("**组合态**渲染 ⇒ 「有新版本 vX」与限定语**同时**出现，且仍可点到 `settings#set-update`", async () => {
    const onNavigate = await renderDash(
      update({
        check_error_app: "GitHub 403 限流（匿名 60 次/小时，按 IP 算）",
        app_update_available: true,
        latest_app: APP,
        checked_at_app: 1,
      }),
    );
    const chip = screen.getByRole("button", { name: /有新版本 v0\.8\.99/ });
    expect(chip.textContent, "必须同时说清限定").toContain("最近一次客户端更新检查没成功");
    expect(chip.textContent, "说清版本号的来源（只有成功那次才会写入 latest_app）").toContain(
      "上次成功查到的",
    );
    expect(chip.textContent, "task-193：现在可以而且应当点名「客户端」").toContain("客户端");
    expect(
      chip.textContent,
      "客户端那一行的失败，不许顺手把核心/geo 也拉进同一句",
    ).not.toContain("核心");
    expect(chip.getAttribute("title"), "原始原因要在 title 里可查").toContain("403");
    expect(screen.queryByText(/已是最新/)).toBeNull();
    fireEvent.click(chip);
    expect(onNavigate).toHaveBeenCalledWith("settings", "set-update");
  });

  it("反例二：**只有**合并字段 `check_error`（核心/geo 失败）⇒ 不得出现任何「没成功」字样（限定语不许常驻）", async () => {
    await renderDash(
      update({
        check_error: "核心更新检查失败：GitHub 超时",
        app_update_available: true,
        latest_app: APP,
        checked_at_app: 1,
      }),
    );
    const chip = screen.getByRole("button", { name: /有新版本 v0\.8\.99/ });
    expect(chip.textContent, "合并字段不得给客户端结论加限定语").not.toContain("没成功");
    expect(document.body.textContent ?? "").not.toContain("客户端更新检查没成功");
  });

  it("**红线（task-193，渲染）**：核心/geo 失败 + 客户端已是最新 ⇒ 显示「客户端已是最新」，**不得**出现「不知道有没有新版本」", async () => {
    await renderDash(
      update({
        check_error: "核心更新检查失败：GitHub 超时",
        latest_app: APP,
        app_update_available: false,
        checked_at: 1,
        checked_at_app: 1,
      }),
    );
    const chip = screen.getByRole("button", { name: /客户端已是最新（v0\.8\.99）/ });
    expect(
      chip.textContent,
      "客户端已确证是最新 —— 核心/geo 的失败不许把它说成「不知道」",
    ).not.toContain("不知道有没有新版本");
    expect(chip.textContent).not.toContain("没成功");
    expect(screen.queryByText(/客户端更新检查没成功/)).toBeNull();
    expect(screen.queryByText(/有新版本 v/), "也不许凭空说有新版").toBeNull();
  });

  it("反例一（渲染）：`check_error` + 无 `latest_app` ⇒ failed，且**不得**出现「有新版本 v」", async () => {
    await renderDash(update({ check_error_app: "离线", latest_app: null, checked_at_app: 1 }));
    expect(screen.getByRole("button", { name: /客户端更新检查没成功/ })).toBeTruthy();
    expect(screen.queryByText(/有新版本 v/)).toBeNull();
    expect(screen.queryByText(/没成功/)).toBeTruthy();
  });

  it("已是最新 ⇒ 显示版本号（不制造焦虑、也不说「有新版本」）", async () => {
    await renderDash(update({ latest_app: APP, app_update_available: false, checked_at_app: 1 }));
    expect(screen.getByRole("button", { name: /客户端已是最新（v0\.8\.99）/ })).toBeTruthy();
    expect(screen.queryByText(/有新版本 v/)).toBeNull();
  });

  it("客户端还没查过 ⇒ 说「还没检查过客户端更新」（**即使合并 `checked_at` 有值**），不显示「已是最新」", async () => {
    // 合并字段有值 = 核心/geo 查过；客户端这条线从没查过 ⇒ 不许当成「查过了」
    await renderDash(update({ checked_at: 1_700_000_000, checked_at_app: null }));
    expect(screen.getByRole("button", { name: /还没检查过客户端更新/ })).toBeTruthy();
    expect(screen.queryByText(/已是最新/)).toBeNull();
  });

  it("反例：**没有任何更新信息**时也**不许**冒出「有新版本」", async () => {
    await renderDash(update({}));
    expect(screen.queryByText(/有新版本 v/)).toBeNull();
  });

  it("提示挂在**仪表盘状态区**里（默认落地页，不必打开设置页）", async () => {
    await renderDash(
      update({ app_update_available: true, latest_app: APP, checked_at_app: 1_700_000_000 }),
    );
    const chip = screen.getByRole("button", { name: /有新版本 v/ });
    const status = document.querySelector(".dash__status") as HTMLElement;
    expect(status, "必须在状态区里，而不是折进提示列表").toBeTruthy();
    expect(status.contains(chip)).toBe(true);
    await waitFor(() => expect(mocks.snapshot).toHaveBeenCalled());
    // 顺带钉住：它没有落在 collectNotices 的「还有 N 条提示」机制里
    expect(document.querySelector(".dash__more")?.textContent ?? "").not.toContain("有新版本");
  });
});
