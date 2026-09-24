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
 * # 三态（+ 两个诚实兜底态）的判据 = `UpdateStatus` 现成字段
 *
 * | 条件 | 结论 |
 * |---|---|
 * | `check_error` 非空 | 「**更新检查没成功** ⇒ 不知道有没有新版本」+ 原因 |
 * | `app_update_available === true` | 「有新版本 vX.Y.Z」 |
 * | 查过 + `latest_app` 有值 + 不可更新 | 「已是最新（vX.Y.Z）」 |
 * | 查过但 `latest_app === null` | 「没拿到版本信息」（**不等于**已是最新） |
 * | `checked_at === null` | 「还没检查过更新」（**不假装**知道结果） |
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
    ...over,
  }) as UpdateStatus;

// ---------------------------------------------------------------------------
// 纯函数：五态
// ---------------------------------------------------------------------------

describe("task-189 · `updateNotice` 的五态", () => {
  it("`app_update_available` ⇒ 才是「有新版本」（只认这个字段）", () => {
    expect(
      updateNotice(update({ app_update_available: true, latest_app: APP, checked_at: 1 })),
    ).toEqual({ kind: "available", version: "0.8.99" });
  });

  it("**有 latest_app 但不可更新** ⇒ 「已是最新」，不许说成「有新版本」", () => {
    expect(updateNotice(update({ latest_app: APP, app_update_available: false, checked_at: 1 }))).toEqual(
      { kind: "latest", version: "0.8.99" },
    );
  });

  it("`check_error` 非空 ⇒ **必须**是 failed（即使 app_update_available 也是 false）", () => {
    const n = updateNotice(
      update({ check_error: "GitHub API 限流（60 次/小时）", checked_at: 1, latest_app: APP }),
    );
    expect(n.kind).toBe("failed");
    expect(n.kind === "failed" && n.reason).toContain("限流");
  });

  it("失败优先：`check_error` + `app_update_available === true` 时也不能说「有新版本」", () => {
    const n = updateNotice(
      update({ check_error: "网络不可达", app_update_available: true, latest_app: APP }),
    );
    expect(n.kind, "没查到就不能下任何结论").toBe("failed");
  });

  it("`checked_at === null` ⇒ unknown（还没查过，不假装知道）", () => {
    expect(updateNotice(update({ checked_at: null })).kind).toBe("unknown");
  });

  it("查过但没拿到版本信息 ⇒ 也是 unknown（不等于已是最新）", () => {
    expect(updateNotice(update({ checked_at: 1, latest_app: null })).kind).toBe("unknown");
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
      update({ app_update_available: true, latest_app: APP, checked_at: 1_700_000_000 }),
    );
    const chip = screen.getByRole("button", { name: /有新版本 v0\.8\.99/ });
    fireEvent.click(chip);
    expect(onNavigate).toHaveBeenCalledWith("settings", "set-update");
  });

  it("**没查到** ⇒ 文案必须看得出是「没查到」，且**不得**出现「已是最新」", async () => {
    await renderDash(update({ check_error: "GitHub API 限流（60 次/小时）", checked_at: 1 }));
    const chip = screen.getByRole("button", { name: /更新检查没成功/ });
    expect(chip.textContent).toContain("不知道有没有新版本");
    expect(chip.getAttribute("title")).toContain("限流");
    expect(screen.queryByText(/已是最新/), "红线：没查到 ≠ 已是最新").toBeNull();
    // 锚定「有新版本 v…」：否则会命中「不知道有**没有新版本**」里的子串（我自己踩到过）
    expect(screen.queryByText(/有新版本 v/)).toBeNull();
  });

  it("已是最新 ⇒ 显示版本号（不制造焦虑、也不说「有新版本」）", async () => {
    await renderDash(update({ latest_app: APP, app_update_available: false, checked_at: 1 }));
    expect(screen.getByRole("button", { name: /已是最新（v0\.8\.99）/ })).toBeTruthy();
    expect(screen.queryByText(/有新版本 v/)).toBeNull();
  });

  it("还没查过 ⇒ 说「还没检查过更新」，不显示「已是最新」", async () => {
    await renderDash(update({ checked_at: null }));
    expect(screen.getByRole("button", { name: /还没检查过更新/ })).toBeTruthy();
    expect(screen.queryByText(/已是最新/)).toBeNull();
  });

  it("反例：**没有任何更新信息**时也**不许**冒出「有新版本」", async () => {
    await renderDash(update({}));
    expect(screen.queryByText(/有新版本 v/)).toBeNull();
  });

  it("提示挂在**仪表盘状态区**里（默认落地页，不必打开设置页）", async () => {
    await renderDash(update({ app_update_available: true, latest_app: APP, checked_at: 1 }));
    const chip = screen.getByRole("button", { name: /有新版本 v/ });
    const status = document.querySelector(".dash__status") as HTMLElement;
    expect(status, "必须在状态区里，而不是折进提示列表").toBeTruthy();
    expect(status.contains(chip)).toBe(true);
    await waitFor(() => expect(mocks.snapshot).toHaveBeenCalled());
    // 顺带钉住：它没有落在 collectNotices 的「还有 N 条提示」机制里
    expect(document.querySelector(".dash__more")?.textContent ?? "").not.toContain("有新版本");
  });
});
