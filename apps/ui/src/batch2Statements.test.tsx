/**
 * task-128：`task-120` 审计里 B 级的第二批（我挑的 5 条）。
 *
 * # 挑了哪 5 条、为什么（按「会不会让人做错事」排序）
 *
 * | # | 位置 | 原来的陈述 | 为什么会让人做错事 |
 * |---|---|---|---|
 * | 1 | `Settings.tsx` 客户端更新 | 检查**失败**之后仍然给「更新到 X 并重启」 | 后端失败时不清 `latest_app`（`snapshot.rs:238-240`），而按钮只看残留值 ⇒ 用户拿一次失败检查的残留版本去升级 |
 * | 2 | `Settings.tsx` 卸载 helper 确认语 | 「会…回滚它装的路由与 DNS」 | 回滚是 helper 收到 SIGTERM 后自己做的；helper 没在跑时**不会发生**，而「已安装但没在跑」正是本页支持的状态 ⇒ 用户以为网络已还原 |
 * | 3 | `Nodes.tsx` 行 title | 「点击切换到该节点」 | `onClick` 在 `busy` 时是 `undefined` ⇒ **点了没反应**（和 B-2 同类：空点击最伤信任） |
 * | 4 | `Logs.tsx` 空态 | 「**刚启动时**这样是正常的」 | 「刚启动」是从 `running` 猜的；跑了几小时后点「清空」也会命中 ⇒ 把用户引向错误原因 |
 * | 5 | `Settings.tsx` DNS 国外组说明 | 「**没连接节点时**这一组显示「未探测」」+「实测 8 秒超时」 | 真实判据是 `spec.socks.is_none()`（＝核心在跑），不是「选了节点」；「8 秒」在代码里查不到（实际探测超时 2s）⇒ 用户按错误的规则预期 |
 */
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  snapshot: vi.fn(),
  tailLogs: vi.fn(),
  addManualNode: vi.fn(),
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
      addManualNode: mocks.addManualNode,
      saveSettings: mocks.saveSettings,
      start: mocks.start,
      stop: mocks.stop,
    },
    subscribe: () => () => {},
  };
});

import Logs from "./pages/Logs";
import Nodes from "./pages/Nodes";
import Settings from "./pages/Settings";
import { scenarioSnapshot } from "./previewSnapshot";
import { StoreProvider } from "./store";
import { formatTimestamp } from "./types";

const APP_UPDATE = {
  size: 12_345_678,
  version: "0.8.99",
  published_at: "2026-01-01T00:00:00Z",
  prerelease: false,
  download_url: "https://example.com/x.zip",
  digest_url: null,
};

function snap(over: Record<string, unknown> = {}) {
  const base = scenarioSnapshot();
  return { ...base, ...over } as never;
}

async function renderWith(snapshot: unknown, ui: React.ReactElement, logs: unknown[] = []) {
  mocks.snapshot.mockResolvedValue(snapshot);
  mocks.tailLogs.mockResolvedValue(logs);
  render(<StoreProvider>{ui}</StoreProvider>);
}

beforeEach(() => {
  vi.clearAllMocks();
  mocks.saveSettings.mockResolvedValue(snap());
  mocks.start.mockResolvedValue(snap());
  mocks.stop.mockResolvedValue(snap());
});

// ---------------------------------------------------------------------------
// 1) B8：检查失败之后不得再劝用户升级
// ---------------------------------------------------------------------------

describe("task-128 · 客户端更新按钮必须看 check_error", () => {
  const withUpdate = (over: Record<string, unknown>) => {
    const base = scenarioSnapshot();
    return snap({ update: { ...base.update, ...over } });
  };

  it("check_error 非空 + 残留 latest_app ⇒ **不给**「更新并重启」，版本号标明是上次查到的", async () => {
    await renderWith(
      withUpdate({
        check_error: "GitHub API 限流（60 次/小时）",
        app_update_available: true,
        latest_app: APP_UPDATE,
      }),
      <Settings focusSection="set-update" />,
    );
    await screen.findByText(/检查更新失败/);
    expect(
      screen.queryByRole("button", { name: /更新到 0\.8\.99 并重启/ }),
      "失败的检查之后不能拿残留版本劝升级",
    ).toBeNull();
    expect(screen.getByText(/上次查到的最新版（本次没查成）/)).toBeTruthy();
  });

  it("反例：check_error 为空 ⇒ 按钮出现，标题是「GitHub 上的最新版」", async () => {
    await renderWith(
      withUpdate({ check_error: null, app_update_available: true, latest_app: APP_UPDATE }),
      <Settings focusSection="set-update" />,
    );
    expect(await screen.findByRole("button", { name: /更新到 0\.8\.99 并重启/ })).toBeTruthy();
    expect(screen.getByText("GitHub 上的最新版")).toBeTruthy();
  });
});

// ---------------------------------------------------------------------------
// 2) B7：卸载确认的后果要看 helper 有没有在跑
// ---------------------------------------------------------------------------

describe("task-128 · 卸载 helper 的确认语必须看 helper 是否在应答", () => {
  const withHelper = (over: Record<string, unknown>) => {
    const base = scenarioSnapshot();
    return snap({ helper: { ...base.helper, ...over } });
  };

  const question = async () => {
    fireEvent.click(await screen.findByRole("button", { name: "卸载 helper" }));
    return (await screen.findByText(/卸载 helper 会/, { exact: false })).textContent ?? "";
  };

  it("helper 没在应答（没在跑）⇒ 必须说清回滚可能不会发生、可能残留后建议先修复网络", async () => {
    await renderWith(
      withHelper({ socket_present: true, reachable: false, state: "not_running" }),
      <Settings focusSection="set-helper" />,
    );
    const text = await question();
    expect(text).toContain("没有在应答");
    expect(text).toContain("残留");
    expect(text).toContain("修复网络");
  });

  it("反例：helper 在应答 ⇒ 保留原来的后果描述，不提残留", async () => {
    await renderWith(
      withHelper({ socket_present: true, reachable: true, state: "ready" }),
      <Settings focusSection="set-helper" />,
    );
    const text = await question();
    expect(text).toContain("回滚它装的路由与 DNS");
    expect(text).not.toContain("残留");
  });
});

// ---------------------------------------------------------------------------
// 3) B2：忙的时候不能继续承诺「点击切换」
// ---------------------------------------------------------------------------

describe("task-128 · 节点行在 busy 时不得继续承诺「点击切换」", () => {
  it("空闲 ⇒ 正常文案；操作进行中 ⇒ 说清为什么点不动（且确实是同一个 busy 判据）", async () => {
    // 让「解析并添加」挂住不结束，`busy` 就会一直是 "add-node"
    let release: (v: unknown) => void = () => {};
    mocks.addManualNode.mockImplementation(
      () => new Promise((res) => (release = res as (v: unknown) => void)),
    );
    await renderWith(snap(), <Nodes />);

    const row = (await screen.findByText("香港 · REALITY 01")).closest(".node-row") as HTMLElement;
    // 反例先立：没在忙时是「已选中…」（task-120 改过的口径）
    expect(row.getAttribute("title")).toContain("已选中");

    fireEvent.click(screen.getByRole("button", { name: "手动添加" }));
    const box = screen.getByPlaceholderText(/三种都行/) as HTMLTextAreaElement;
    fireEvent.change(box, { target: { value: "vless://x@example.com:443" } });
    fireEvent.click(screen.getByRole("button", { name: "解析并添加" }));

    await waitFor(() => expect(mocks.addManualNode).toHaveBeenCalledTimes(1));
    const busyRow = document.querySelector(".node-row") as HTMLElement;
    expect(busyRow.getAttribute("title")).toBe("操作进行中，暂时不能切换节点");
    expect(busyRow.getAttribute("title")).not.toContain("点击切换");
    release({});
  });
});

// ---------------------------------------------------------------------------
// 4) B9：日志空态不能靠猜「刚启动」
// ---------------------------------------------------------------------------

describe("task-128 · 日志空态必须摆出启动时刻，而不是猜原因", () => {
  const startedAt = 1_700_000_000;
  const withRuntime = (over: Record<string, unknown>) => {
    const base = scenarioSnapshot();
    return snap({ runtime: { ...base.runtime, ...over } });
  };

  it("核心在跑 ⇒ 渲染出 started_at_unix 对应的时刻，且不再说「刚启动时这样是正常的」", async () => {
    await renderWith(withRuntime({ running: true, started_at_unix: startedAt }), <Logs />);
    expect(await screen.findByText(/本次启动于/)).toBeTruthy();
    const text = (document.querySelector(".logs__empty") as HTMLElement).textContent ?? "";
    expect(text).toContain(formatTimestamp(startedAt));
    expect(text).toContain("也可能是日志刚被清空");
    expect(screen.queryByText(/刚启动时这样是正常的/)).toBeNull();
  });

  it("反例：started_at_unix 读不到 ⇒ 一个时刻都不编", async () => {
    await renderWith(withRuntime({ running: true, started_at_unix: null }), <Logs />);
    await screen.findByText(/核心已在运行/);
    const text = (document.querySelector(".logs__empty") as HTMLElement).textContent ?? "";
    expect(text, "读不到就说读不到").toContain("读不到本次的启动时刻");
    expect(text, "不许编一个时刻出来").not.toContain("本次启动于");
    expect(text).not.toContain("刚启动时这样是正常的");
  });
});

// ---------------------------------------------------------------------------
// 5) B17：DNS 国外组「未探测」的真实条件
// ---------------------------------------------------------------------------

describe("task-128 · DNS 国外组的说明必须按 runtime.running 说", () => {
  const withRuntime = (running: boolean) => {
    const base = scenarioSnapshot();
    return snap({ runtime: { ...base.runtime, running } });
  };

  it("核心在跑 ⇒ 说「真的在探测」；不再写「没连接节点时就显示未探测」", async () => {
    await renderWith(withRuntime(true), <Settings focusSection="set-dns-probe" />);
    expect(await screen.findByText(/真的在探测/)).toBeTruthy();
    expect(screen.queryByText(/没连接节点时这一组显示/)).toBeNull();
  });

  it("反例：核心没在跑 ⇒ 说「没有探测」而不是「真的在探测」", async () => {
    await renderWith(withRuntime(false), <Settings focusSection="set-dns-probe" />);
    expect(await screen.findByText(/没有探测/)).toBeTruthy();
    expect(screen.queryByText(/真的在探测/)).toBeNull();
  });
});
