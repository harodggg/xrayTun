/**
 * task-192：`app://update-checked` 必须有一个**真实消费者**。
 *
 * # 这条测试防的故障
 *
 * `task-188` 让 App 启动 20 s 后自动查一次客户端最新版、之后每 6 h 复查，并新增事件
 * `APP_UPDATE_CHECKED = "app://update-checked"`（`apps/desktop/src/events.rs:27`）。
 * 但前端只在挂载 + `nodes://changed` / `subscriptions://changed` / `settings://changed`
 * 时拉快照 ⇒ **20 秒后查到的结果要等下一次那几个事件之一才显示** ——
 * 「自动检测」在用户眼里可能等于「什么都没发生」。这个功能的价值全在这一条订阅上。
 *
 * # 为什么这里 mock 的是 **Tauri 边界**（`@tauri-apps/api/{core,event}`），而不是 `./ipc`
 *
 * 既有的事件测试（`snapshotShapeGuard.test.tsx`）把整个 `./ipc` 换成手写 mock、
 * 捕获 `subscribe()` 的 handlers 再手动调。那种做法**测不到事件名**：
 * 只要 `store.tsx` 传了 `onAppUpdateChecked`，mock 就「捕获到了 handler」⇒ 测试全绿；
 * 而如果 `ipc.ts::subscribe` 忘了给它配对事件名（或名字拼错），真实运行时
 * **永远收不到事件** —— 那正是本卡要修的那个故障（Tauri 里事件名拼错不报错，
 * 只会静静地收不到）。所以这里只用 mock 顶住 Tauri 边界，让**真实**的 `ipc.ts`
 * 与**真实**的 `store.tsx` 一起跑，断言的是「注册到 Tauri 的监听名」本身。
 */
import { act, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  /** Tauri 的 `invoke`：记录每个被调用的命令。 */
  invoke: vi.fn(),
  /** Tauri 的 `listen`：按事件名抓住后端推来的 handler（测试里手动触发）。 */
  listeners: new Map<string, (e: { payload: unknown }) => void>(),
  /** 当前这份快照 —— 测试里改它来模拟「自动检测已经写进后端状态」。 */
  snap: null as unknown,
}));

vi.mock("@tauri-apps/api/core", () => ({ invoke: mocks.invoke }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: async (name: string, handler: (e: { payload: unknown }) => void) => {
    mocks.listeners.set(name, handler);
    return () => {
      mocks.listeners.delete(name);
    };
  },
}));

import { EVENTS } from "./ipc";
import { scenarioSnapshot } from "./previewSnapshot";
import { StoreProvider, useStore } from "./store";

const GOOD = scenarioSnapshot();

/** 探针：把快照里一个可见的值暴露出来，用于断言「界面真的换了新数据」。 */
function Probe() {
  const { snapshot } = useStore();
  return <span data-testid="rev">{snapshot ? snapshot.app_version : "none"}</span>;
}

/** 只数 `snapshot` 这条命令 —— 挂载时还会拉日志（`tail_logs`）。 */
const snapshotCalls = () => mocks.invoke.mock.calls.filter((c) => c[0] === "snapshot").length;

async function mount() {
  render(
    <StoreProvider>
      <Probe />
    </StoreProvider>,
  );
  await waitFor(() => expect(screen.getByTestId("rev").textContent).toBe(GOOD.app_version));
}

/** 触发后端事件 —— 走的是 `ipc.ts::subscribe` **真实注册**进 Tauri 的那条 handler。 */
async function emit(name: string, payload: unknown = {}) {
  const handler = mocks.listeners.get(name);
  expect(handler, `没有向 Tauri 注册 ${name} 的监听`).toBeTypeOf("function");
  await act(async () => {
    handler!({ payload });
  });
}

beforeEach(() => {
  vi.clearAllMocks();
  mocks.listeners.clear();
  mocks.snap = GOOD;
  mocks.invoke.mockImplementation(async (cmd: string) =>
    cmd === "snapshot" ? mocks.snap : [],
  );
});

describe("task-192 · 自动版本检测的事件必须有真实消费者", () => {
  it("映射就是字面量 `app://update-checked`（与 events.rs::APP_UPDATE_CHECKED 一致，**不是** `nodes://changed` 的别名）", () => {
    expect(EVENTS.appUpdateChecked).toBe("app://update-checked");
    // 红线：不许用「借一个不相关的事件骗刷新」——那会让语义撒谎（节点没变却说变了）。
    expect(EVENTS.appUpdateChecked).not.toBe(EVENTS.nodesChanged);
  });

  it("真实 `ipc.ts::subscribe` 向 Tauri 注册了 `app://update-checked` 这个名（拼错就永远收不到）", async () => {
    await mount();
    expect([...mocks.listeners.keys()]).toContain("app://update-checked");
  });

  it("字段=X ⇒ 呈现=Y：收到该事件 ⇒ **会重新拉快照**，界面随之更新", async () => {
    await mount();
    const before = snapshotCalls();

    // 模拟「20 s 后自动检测完成了，后端状态已变」——只有重拉快照才能看见它。
    mocks.snap = { ...GOOD, app_version: "9.9.9" };
    await emit(EVENTS.appUpdateChecked, {
      latest_app: null,
      checked_at: 1_800_000_000,
      check_error: "GitHub 403 限流",
    });

    await waitFor(() => expect(screen.getByTestId("rev").textContent).toBe("9.9.9"));
    expect(snapshotCalls(), "一次事件只该重拉一次").toBe(before + 1);
  });

  it("反例：载荷是任意坏形状也照样刷新（载荷**不被读**，真源只有快照）", async () => {
    await mount();
    const before = snapshotCalls();
    // 失败也发事件（`events.rs` 的注释），而且我们**不读**载荷 ⇒ 不需要为新载荷加守卫，
    // 也不会因为载荷畸形而漏掉刷新。
    await emit(EVENTS.appUpdateChecked, null);
    await waitFor(() => expect(snapshotCalls()).toBe(before + 1));
  });

  it("反例：**没有**该事件时不会自己多拉快照（证明上面那次刷新确实来自事件）", async () => {
    await mount();
    const before = snapshotCalls();
    // 让所有已注册的监听器都「空转」一段时间：不触发事件就不该有新快照请求。
    await act(async () => {
      await new Promise((r) => setTimeout(r, 60));
    });
    expect(snapshotCalls()).toBe(before);
  });
});
