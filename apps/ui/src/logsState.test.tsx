/**
 * 日志页「读不到 ≠ 没有」的回归测试（task-23 缺陷 A）。
 *
 * # 背景
 *
 * `store.tsx` 里原来是 `.catch(() => { /* 静默即可 *\/ })`：读失败与「真的没有日志」
 * 在界面上完全一样，而空态文案又把空列表解释成「核心还没启动过」—— 用户被告知的是
 * **错误的原因**。本项目在流量字节数、连接数、域名配对上修过三次同类问题，
 * 这是第四处，也是唯一一处「给错因」的。
 *
 * # 这几条测试钉住什么
 *
 * 1. 失败态：有原因、有可重试的动作，且**不出现**「核心还没启动过」；
 * 2. 「核心没在跑」与「核心在跑但还没输出」是两句不同的话，判据是快照的
 *    `runtime.running`（后端真实值），不是前端猜；
 * 3. 三种说法互不相同 —— 防止将来又把它们合并成一句；
 * 4. 「重试」真的再取一次（不是装饰按钮）；
 * 5. 清空失败时界面**不能**装作已清空（日志文件还在，刷新就会回来）。
 */
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  snapshot: vi.fn(),
  tailLogs: vi.fn(),
  clearLogs: vi.fn(),
  diagnostics: vi.fn(),
}));

vi.mock("./ipc", () => ({
  api: {
    snapshot: mocks.snapshot,
    tailLogs: mocks.tailLogs,
    clearLogs: mocks.clearLogs,
    diagnostics: mocks.diagnostics,
  },
  errorText: (e: unknown) =>
    typeof e === "string" ? e : e instanceof Error ? e.message : String(e),
  subscribe: () => () => {},
}));

import Logs from "./pages/Logs";
import { scenarioSnapshot } from "./previewSnapshot";
import { StoreProvider } from "./store";

/**
 * 本页只关心 `runtime.running`，但快照**仍然要给完整的**：
 * 部分形状的替身是「夹具在说谎」—— 页面今天只读这一个字段，明天多读一个就会
 * 以 unhandled error 的形式炸掉（`Errors N` 会让退出码变 1，而通过数看不出来）。
 */
function snap(running: boolean, startedAt: number | null = running ? 1_700_000_000 : null) {
  const base = scenarioSnapshot();
  // `started_at_unix` 必须**显式**表达场景：空态文案的判据是它（task-128 的 B9），
  // 而不是「字段恰好缺席」—— 原来那个部分形状的替身正是因为没写这个字段，
  // 才让「核心没在跑」蒙对了「还没启动过」这句。
  //   * 没在跑 ⇒ null（确实没启动过）
  //   * 在跑   ⇒ 给一个真实时刻（于是走「已运行、但当前没有日志」那一支）
  return { ...base, runtime: { ...base.runtime, running, started_at_unix: startedAt } } as never;
}

function renderLogs() {
  return render(
    <StoreProvider>
      <Logs />
    </StoreProvider>,
  );
}

beforeEach(() => {
  vi.clearAllMocks();
  mocks.snapshot.mockResolvedValue(snap(true));
  mocks.clearLogs.mockResolvedValue(undefined);
  mocks.diagnostics.mockResolvedValue("（诊断报告正文）");
});

describe("日志页：读取失败不许说成「没有日志」（task-23 A）", () => {
  it("失败态给出后端原文 + 可重试，且不出现「核心还没启动过」", async () => {
    mocks.tailLogs.mockRejectedValue(new Error("Permission denied (os error 13)"));
    renderLogs();

    const banner = await screen.findByRole("alert");
    expect(banner.textContent).toContain("读取日志失败");
    expect(banner.textContent).toContain("Permission denied (os error 13)");
    expect(screen.getByRole("button", { name: "重试" })).toBeTruthy();
    // 核心断言：读失败与「核心没启动」必须是两回事
    expect(screen.queryByText(/核心还没启动过/)).toBeNull();
  });

  it("读取成功 + 空 + 核心没在跑 → 「核心还没启动过」", async () => {
    mocks.tailLogs.mockResolvedValue([]);
    mocks.snapshot.mockResolvedValue(snap(false));
    renderLogs();

    expect(await screen.findByText(/核心还没启动过/)).toBeTruthy();
    expect(screen.queryByRole("alert")).toBeNull();
  });

  // task-128：这一格的文案从「还没有产生日志」改成如实摆出启动时刻 + 不猜原因
  // （原来是「刚启动时这样是正常的」，而「刚启动」是从 running 猜的）。
  // 断言强度不变：仍然要求它与另外两种「空」是**不同的说法**。
  it("读取成功 + 空 + 核心在跑 → 说清「当前还没有日志」，不赖核心没启动", async () => {
    mocks.tailLogs.mockResolvedValue([]);
    renderLogs();

    expect(await screen.findByText(/但当前还没有日志/)).toBeTruthy();
    expect(screen.queryByText(/核心还没启动过/)).toBeNull();
  });

  it("三种「空」的说法互不相同（防止将来又被合并成一句）", async () => {
    const texts: string[] = [];

    mocks.tailLogs.mockRejectedValueOnce(new Error("boom"));
    const a = renderLogs();
    texts.push((await screen.findByText(/这不等于「没有日志」/)).textContent ?? "");
    a.unmount();

    mocks.tailLogs.mockResolvedValue([]);
    mocks.snapshot.mockResolvedValue(snap(false));
    const b = renderLogs();
    texts.push((await screen.findByText(/核心还没启动过/)).textContent ?? "");
    b.unmount();

    mocks.snapshot.mockResolvedValue(snap(true));
    const c = renderLogs();
    texts.push((await screen.findByText(/但当前还没有日志/)).textContent ?? "");
    c.unmount();

    expect(new Set(texts).size).toBe(3);
  });

  it("「重试」真的再取一次，成功后横幅消失并换成当前状态的文案", async () => {
    mocks.tailLogs.mockRejectedValueOnce(new Error("boom"));
    renderLogs();
    await screen.findByRole("alert");

    mocks.tailLogs.mockResolvedValue([]);
    fireEvent.click(screen.getByRole("button", { name: "重试" }));

    await waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
    expect(await screen.findByText(/但当前还没有日志/)).toBeTruthy();
    expect(mocks.tailLogs).toHaveBeenCalledTimes(2);
  });

  it("清空失败：界面不能装作已清空（后端说没删掉，日志就还得在）", async () => {
    mocks.tailLogs.mockResolvedValue([
      { ts_unix: 1, source: "app", level: "info", message: "既有日志" },
    ]);
    mocks.clearLogs.mockRejectedValue(new Error("Permission denied"));
    renderLogs();

    expect(await screen.findByText("既有日志")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "清空" }));
    fireEvent.click(screen.getByRole("button", { name: "确认清空" }));

    await waitFor(() => expect(mocks.clearLogs).toHaveBeenCalledTimes(1));
    // 失败 → 不能清空界面：那条日志必须还在
    expect(screen.getByText("既有日志")).toBeTruthy();
  });
});

// ---------------------------------------------------------------------------
// D2：等级的一个字符标记（E/W/I/D）—— 颜色之外的第二编码
//
// 原来等级只有「消息文字颜色 + 2px 左色条」，两者都是颜色 ⇒ 色觉障碍用户（以及灰度
// 截图、低质量投影）四个等级读起来一样。这几条钉住「第二编码真的在 DOM 里」，
// 并且钉住它**没有**顺手改版式（不新增列、不新增 DOM 节点）。
// ---------------------------------------------------------------------------

describe("D2：日志等级有一个字符的第二编码（不靠颜色）", () => {
  /** 四个等级各一行；`helper` 是最长的来源名（`.log-line__src` 定宽 44px 就是为它定的）。 */
  const fourLevels = [
    { ts_unix: 1, source: "core", level: "error", message: "出错了" },
    { ts_unix: 2, source: "app", level: "warn", message: "注意" },
    { ts_unix: 3, source: "helper", level: "info", message: "正常" },
    { ts_unix: 4, source: "core", level: "debug", message: "细节" },
  ];

  /** 行首的等级字符：从 DOM 的文本读，不 import 实现的常量。 */
  const markerOf = (row: Element): string =>
    (row.querySelector(".log-line__ts")?.textContent ?? "").trimStart().charAt(0);

  it("每一行都有一个与等级对应的字符，四个等级四个不同字符", async () => {
    mocks.tailLogs.mockResolvedValue(fourLevels);
    const { container } = renderLogs();
    await screen.findByText("出错了");

    const rows = [...container.querySelectorAll(".log-line")];
    expect(rows.length, "四行应当都渲染出来").toBe(4);
    expect(rows.map(markerOf)).toEqual(["E", "W", "I", "D"]);
    // 「第二编码」的本义：即使只剩灰度，四个等级仍然互不相同
    expect(new Set(rows.map(markerOf)).size).toBe(4);
  });

  it("未知等级给「·」，不按颜色猜一个等级出来", async () => {
    mocks.tailLogs.mockResolvedValue([
      { ts_unix: 9, source: "core", level: "trace", message: "未知等级" },
    ]);
    const { container } = renderLogs();
    await screen.findByText("未知等级");

    const row = container.querySelector(".log-line")!;
    expect(markerOf(row)).toBe("·");
  });

  it("不加列、不加节点：行仍是 ts/src/msg 三个子元素，字符挂在定宽时间戳列里", async () => {
    mocks.tailLogs.mockResolvedValue(fourLevels);
    const { container } = renderLogs();
    await screen.findByText("出错了");

    const rows = [...container.querySelectorAll<HTMLElement>(".log-line")];
    expect(rows.length).toBe(4);
    for (const row of rows) {
      // ① 子元素数不变（等级字符不是第四个 flex 子项 ⇒ 列宽与列间距都没变）
      expect([...row.children].map((c) => c.className)).toEqual([
        "log-line__ts",
        "log-line__src",
        "log-line__msg",
      ]);
      // ② 字符在时间戳列内部；来源列仍完整保留（helper 不许被挤掉）
      expect(row.querySelector(".log-line__ts")!.textContent).toMatch(/^[EWID·] \d\d:\d\d:\d\d$/);
      // ③ **文本节点数也不许变**：加了等级字符之后，时间戳列仍然只有 1 个文本节点
      //   （原来 `{formatClock(...)}` 就是 1 个）。写成分成两个表达式会让它变成 3 个，
      //   而这页是 1500 行、`logsDomStability.test.tsx` 会逐行重建整棵树 ——
      //   逐行开销会被放大，所以这条是「零额外开销」的结构判据。
      expect(row.querySelector(".log-line__ts")!.childNodes.length).toBe(1);
      expect(row.querySelector(".log-line__src")!.textContent).toBe(
        fourLevels[rows.indexOf(row)]!.source,
      );
    }
  });
});

// ---------------------------------------------------------------------------
// F3：日志诊断面板的 `Esc` 关闭（与拓扑页同一个键盘约定，dev-ia task-8 请求）
// ---------------------------------------------------------------------------
describe("F3：诊断面板按 Esc 关闭", () => {
  it("面板打开时 Esc 关闭它；面板没打开时 Esc 不产生副作用", async () => {
    mocks.tailLogs.mockResolvedValue([
      { ts_unix: 1, source: "core", level: "info", message: "既有日志" },
    ]);
    renderLogs();
    await screen.findByText("既有日志");

    // 面板没打开：Esc 不该变出什么（别的组件，如 InlineConfirm，也要处理 Esc）
    fireEvent.keyDown(window, { key: "Escape" });
    expect(screen.queryByText("诊断报告")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "诊断" }));
    expect(await screen.findByText("诊断报告")).toBeTruthy();

    fireEvent.keyDown(window, { key: "Escape" });
    await waitFor(() => expect(screen.queryByText("诊断报告")).toBeNull());
  });
});

// ---------------------------------------------------------------------------
// B5：日志页复制成功必须给 `role="status"` 反馈（用户可感知）
//
// 0.9-PLAN §3 B5 的判据是「复制成功给 role=status 反馈」。实现走的是共用
// `CopyButton`（task-124）；失败那一半由 `a11yQuickFixes.test.tsx` 钉着（剪贴板被拒
// ⇒ `role=alert` + 可手动选中的 textarea），这里补上**成功**那一半：用户必须能看出
// 「刚才那一下成了」，而不是重复点。
//
// ⚠️ 诚实边界：这条钉的是**有反馈**，不是审计原文里的「2 秒 / 已复制 N 行」措辞。
// 后者要改 `IncidentReport.tsx` 的 `CopyButton`（不在本卡写范围），见交给 lead 的说明。
// ---------------------------------------------------------------------------

describe("B5：日志页复制成功有 role=status 反馈", () => {
  it("剪贴板成功 ⇒ 播报「已复制」，且不出现失败块；写进去的就是筛选后的日志原文", async () => {
    const line = { seq: 1, ts_unix: 1, source: "core", level: "info", message: "既有日志" };
    mocks.tailLogs.mockResolvedValue([line]);
    const writeText = vi.fn(() => Promise.resolve());
    Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });

    renderLogs();
    await screen.findByText("既有日志");
    fireEvent.click(await screen.findByRole("button", { name: "复制" }));

    const status = await screen.findByText(/已复制/);
    expect(status.getAttribute("role"), "成功反馈必须进 live region（role=status）").toBe("status");
    expect(screen.queryByText(/没有复制成功/)).toBeNull();
    // 复制的内容与界面同源：不是空的、也不是别的什么东西
    expect(writeText).toHaveBeenCalledWith(
      `[${new Date(line.ts_unix * 1000).toISOString()}] ${line.source}/${line.level} ${line.message}`,
    );

    Object.defineProperty(navigator, "clipboard", { value: undefined, configurable: true });
  });
});
