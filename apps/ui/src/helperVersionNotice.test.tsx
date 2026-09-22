/**
 * 助手版本不一致的可见提示（task-84 的 Rust 半 → **task-86 的 UI 半**）。
 *
 * # 问题（一句话）
 *
 * **App 更新不会刷新特权助手**：`restart_helper` 只 kickstart 磁盘上那份**旧**二进制，
 * 只有 `install_helper` 才把包内那份拷过去；而**路由/DNS 的安装与回滚都在 helper 里**
 * ⇒ 两者不一致时，用户以为「更新拿到了全部修复」，**helper 侧那部分其实没生效**，
 * 而且**完全无声**（只校验协议号，不校验版本）。这正是本项目最忌讳的「静默无效」。
 *
 * # 这一组钉住什么（三态必须分开，不许合并）
 *
 * 1. `mismatch`   → 提示出现、写清**两个版本号**、含「重新安装助手」入口；
 * 2. `match`      → **什么都不显示**（反例：不许「只要检测就提示」，否则狼来了）；
 * 3. `unreadable` → **如实说读不到**（原样带上后端给的原因），既不提示重装、也不说一致；
 * 4. 点重装 → **走到 install 通路**（调用序列断言），且**渲染时绝不自动重装**
 *    —— 那是特权操作（要管理员授权），必须用户点；
 * 5. 预览快照还没有这个字段（已知保真度缺口）→ **什么都不显示且不崩**
 *    （缺字段 ≠ match ≠ mismatch，猜任何一个都是编造）。
 */
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  snapshot: vi.fn(),
  tailLogs: vi.fn(),
  saveSettings: vi.fn(),
  installHelper: vi.fn(),
  restartHelper: vi.fn(),
  uninstallHelper: vi.fn(),
  restoreStale: vi.fn(),
}));

vi.mock("./ipc", () => ({
  api: {
    snapshot: mocks.snapshot,
    tailLogs: mocks.tailLogs,
    saveSettings: mocks.saveSettings,
    installHelper: mocks.installHelper,
    restartHelper: mocks.restartHelper,
    uninstallHelper: mocks.uninstallHelper,
    restoreStale: mocks.restoreStale,
  },
  errorText: (e: unknown) =>
    typeof e === "string" ? e : e instanceof Error ? e.message : String(e),
  parseRecovery: () => null,
  subscribe: () => () => {},
}));

import Settings from "./pages/Settings";
import { scenarioSnapshot } from "./previewSnapshot";
import { StoreProvider } from "./store";
import type { HelperVersionCheck } from "./types";

/** 造快照：`version_check` 传 `undefined` 表示「字段缺失」（= 预览快照的现状）。 */
function snapWithCheck(check: HelperVersionCheck | undefined) {
  const base = scenarioSnapshot();
  const helper: Record<string, unknown> = {
    ...base.helper,
    // 用「已安装且就绪」的常态，避免与「未安装」那条空态文案混在一起判断
    state: "ready",
    socket_present: true,
    reachable: true,
    version: check?.state === "mismatch" ? check.installed : "0.8.31",
  };
  if (check === undefined) delete helper.version_check;
  else helper.version_check = check;
  return { ...base, helper } as never;
}

async function renderHelper(check: HelperVersionCheck | undefined) {
  mocks.snapshot.mockResolvedValue(snapWithCheck(check));
  mocks.tailLogs.mockResolvedValue([]);
  const r = render(
    <StoreProvider>
      <Settings focusSection="set-helper" />
    </StoreProvider>,
  );
  // 等「特权助手」节渲染出来（用标题当锚点：按钮里带 "helper" 的有好几个）
  await screen.findByRole("heading", { name: /特权助手/ });
  return r;
}

const notice = () => screen.queryByRole("status");
const reinstall = () => screen.queryByRole("button", { name: "重新安装助手" });

const MISMATCH: HelperVersionCheck = {
  state: "mismatch",
  installed: "0.8.31",
  bundled: "0.8.32",
};
const MATCH: HelperVersionCheck = { state: "match", version: "0.8.32" };
const UNREADABLE: HelperVersionCheck = {
  state: "unreadable",
  installed: null,
  bundled: "0.8.32",
  // 后端 `classify_helper_versions` 的原文
  reason: "读不到已安装助手的版本（文件不存在或无法执行）",
};

beforeEach(() => {
  vi.clearAllMocks();
  mocks.installHelper.mockResolvedValue(snapWithCheck(MATCH));
  mocks.restartHelper.mockResolvedValue(snapWithCheck(MATCH));
  mocks.saveSettings.mockResolvedValue(snapWithCheck(MATCH));
});

describe("助手版本不一致的提示（task-86）", () => {
  it("**不一致 ⇒ 提示出现**：写清两个版本号，并给出「重新安装助手」入口", async () => {
    const { container } = await renderHelper(MISMATCH);

    expect(notice(), "版本不一致必须说出来").not.toBeNull();
    const text = notice()!.textContent ?? "";
    expect(text, "必须给出已安装的版本").toContain("0.8.31");
    expect(text, "必须给出随 App 附带的版本").toContain("0.8.32");
    expect(text, "必须说清为什么"); // 下面两条是「为什么」的具体内容
    expect(text).toContain("助手负责安装路由与 DNS");
    expect(text).toContain("助手侧的这部分修复不会生效");
    // 不许夸大：主修复在 App 侧，不能写成「不重装就什么都无效」
    expect(text, "不许写成「什么都无效」").toContain("其余修复不受影响");

    expect(reinstall(), "提示里必须有重装入口").not.toBeNull();
    // 提示要出现在「特权助手」节里（与安装/重启/卸载同一处）
    expect(container.querySelector("#set-helper")!.contains(notice()!)).toBe(true);
    // ⚠️ 渲染时**绝不**自动重装（特权操作必须用户点）
    expect(mocks.installHelper, "不许静默自动重装").not.toHaveBeenCalled();
  });

  it("**反例：一致 ⇒ 什么都不显示**（不许「只要检测就提示」）", async () => {
    await renderHelper(MATCH);
    expect(notice(), "版本一致却提示了 —— 狼来了").toBeNull();
    expect(reinstall(), "版本一致却给了重装入口").toBeNull();
    expect(screen.queryByText(/无法核对助手版本/), "版本一致却说读不到").toBeNull();
  });

  it("**读不到 ⇒ 如实说读不到**：原样带出后端给的原因，不猜成一致、也不提示重装", async () => {
    await renderHelper(UNREADABLE);

    expect(notice(), "读不到时不许当成「不一致」去吓人").toBeNull();
    expect(reinstall(), "读不到时不该给重装入口").toBeNull();
    const hint = screen.getByText(/无法核对助手版本/).textContent ?? "";
    expect(hint, "必须原样说清读不到的原因").toContain(UNREADABLE.reason);
    expect(hint, "必须说清这里既不说一致也不说不一致").toContain("既不说");
  });

  it("点「重新安装助手」⇒ 走到**既有 install 通路**，且只调它一个（调用序列）", async () => {
    await renderHelper(MISMATCH);

    fireEvent.click(screen.getByRole("button", { name: "重新安装助手" }));
    await waitFor(() => expect(mocks.installHelper).toHaveBeenCalledTimes(1));
    // 复用既有通路：不是自造机制、也不是重启/卸载
    expect(mocks.restartHelper, "重装不该走重启（重启不更新二进制）").not.toHaveBeenCalled();
    expect(mocks.uninstallHelper).not.toHaveBeenCalled();
  });

  it("字段缺失（预览快照的现状）⇒ 什么都不显示且不崩 —— 缺字段不等于任何一态", async () => {
    await renderHelper(undefined);
    expect(notice()).toBeNull();
    expect(reinstall()).toBeNull();
    expect(screen.queryByText(/无法核对助手版本/)).toBeNull();
  });
});
