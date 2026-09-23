/**
 * task-155：B 级批次 4（我自己点名的「未挑里最危险的 3 条」）。
 *
 * | # | 位置 | 原来的陈述 | 为什么会误导 |
 * |---|---|---|---|
 * | 1 | `Subscriptions.tsx` 余量结论 | 超额时仍写「**即将**用尽」 | `ratio` 被 `Math.min(1, …)` 压到 1 ⇒ **分不出「刚好用完」与「超额」**；用户以为还有余量 ⇒ 继续用 / 不续费 |
 * | 2 | `Nodes.tsx` 删除确认 | 「下次更新订阅时**会**重新出现」 | 后端只在「抓取成功 **且** 解析成功」时才重新导入；抓取失败或上游删掉它就不会回来 —— **无限定承诺** |
 * | 3 | `DestChecker.tsx` 判定结论 | 用 `rule_tag` 判「有没有命中」 | 「命中」的权威判据是 `rule_index`（`explain.rs:274`）；`rule_tag` 可空 ⇒ 会把「命中了第 N 条」说成「**未命中任何规则**」，而屏幕上的出站其实是那条规则的出站（不是兜底） |
 */
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  snapshot: vi.fn(),
  tailLogs: vi.fn(),
  saveSettings: vi.fn(),
  start: vi.fn(),
  stop: vi.fn(),
  explainDest: vi.fn(),
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
      explainDest: mocks.explainDest,
    },
    subscribe: () => () => {},
  };
});

import Nodes from "./pages/Nodes";
import Subscriptions from "./pages/Subscriptions";
import { scenarioSnapshot } from "./previewSnapshot";
import { StoreProvider } from "./store";
import { DestChecker } from "./topology/DestChecker";

const usageSnap = (used: number, total: number) => {
  const base = scenarioSnapshot();
  return {
    ...base,
    subscriptions: [
      {
        id: "sub-1",
        name: "主订阅 · 机场 A",
        url: "https://example.com/sub",
        enabled: true,
        update_interval_hours: 24,
        last_updated: 1_700_000_000,
        last_error: null,
        node_count: 1,
        // 头字段是真的：upload + download 由后端解析得到
        usage: { upload: Math.floor(used / 2), download: used - Math.floor(used / 2), total, expire: null },
      },
    ],
  } as never;
};

beforeEach(() => {
  vi.clearAllMocks();
  mocks.saveSettings.mockResolvedValue(scenarioSnapshot());
  mocks.snapshot.mockResolvedValue(scenarioSnapshot());
  mocks.tailLogs.mockResolvedValue([]);
});

// ---------------------------------------------------------------------------
// 1) 超额 / 用尽 / 偏低 三档分开
// ---------------------------------------------------------------------------

describe("task-155 · 余量结论：超额不许再说「即将用尽」", () => {
  async function renderWithUsage(used: number, total: number) {
    mocks.snapshot.mockResolvedValue(usageSnap(used, total));
    render(
      <StoreProvider>
        <Subscriptions />
      </StoreProvider>,
    );
    await screen.findByText("主订阅 · 机场 A");
    return document.body.textContent ?? "";
  }

  it("**超额** ⇒ 说「已超额」并给出超出多少；不得出现「即将用尽」", async () => {
    const text = await renderWithUsage(120 * 1024 * 1024, 100 * 1024 * 1024);
    expect(text).toContain("已超额");
    expect(text, "超出量要写出来").toContain("20.00 MiB");
    expect(text, "超额了就不能再说「即将」").not.toContain("即将用尽");
  });

  it("刚好用尽 ⇒ 「已用尽」，也不是「即将用尽」", async () => {
    const text = await renderWithUsage(100 * 1024 * 1024, 100 * 1024 * 1024);
    expect(text).toContain("已用尽");
    expect(text).not.toContain("即将用尽");
  });

  it("反例：95% ⇒ 仍然是「即将用尽」（没被一起改掉）", async () => {
    const text = await renderWithUsage(95 * 1024 * 1024, 100 * 1024 * 1024);
    expect(text).toContain("即将用尽");
    expect(text).not.toContain("已超额");
  });

  it("反例：80% ⇒ 「余量偏低」", async () => {
    expect(await renderWithUsage(80 * 1024 * 1024, 100 * 1024 * 1024)).toContain("余量偏低");
  });

  it("反例：50% ⇒ 什么都不说（不制造焦虑）", async () => {
    const half = await renderWithUsage(50 * 1024 * 1024, 100 * 1024 * 1024);
    expect(half).not.toContain("余量偏低");
    expect(half).not.toContain("即将用尽");
    expect(half).not.toContain("已超额");
  });

  it("`total === 0`（不限量）⇒ 不产生任何余量结论", async () => {
    const text = await renderWithUsage(500 * 1024 * 1024, 0);
    expect(text).toContain("不限量");
    expect(text).not.toContain("已用尽");
    expect(text).not.toContain("已超额");
  });
});

// ---------------------------------------------------------------------------
// 2) 订阅来源节点的删除承诺
// ---------------------------------------------------------------------------

describe("task-155 · 删除订阅来源的节点：只承诺能保证的", () => {
  it("必须写成有条件的（抓取/解析成功才回来），不得是无限定承诺", async () => {
    const base = scenarioSnapshot();
    mocks.snapshot.mockResolvedValue({
      ...base,
      nodes: [
        { ...base.nodes[0]!, id: "n-sub-1", name: "节点 1", source: { kind: "subscription", id: "sub-1" } },
      ],
    } as never);
    render(
      <StoreProvider>
        <Nodes />
      </StoreProvider>,
    );
    fireEvent.click(await screen.findByRole("button", { name: "删除" }));
    const question = (await screen.findByText(/删除节点「节点 1」/)).textContent ?? "";
    expect(question).toContain("如果下次更新能成功抓取并解析到它");
    expect(question, "抓取失败/上游删除要写出来").toContain("上游把它删掉就不会");
    expect(
      question,
      "不许再出现那句无限定的承诺",
    ).not.toContain("下次更新订阅时会重新出现");
  });
});

// ---------------------------------------------------------------------------
// 3) 判定结论：命中但无规则名 ≠ 未命中
// ---------------------------------------------------------------------------

describe("task-155 · 判定器：用 `rule_index` 判「有没有命中」", () => {
  async function explainWith(result: unknown) {
    mocks.explainDest.mockResolvedValue(result);
    render(<DestChecker geoAvailable />);
    fireEvent.change(screen.getByPlaceholderText(/www\.google\.com/), {
      target: { value: "example.com" },
    });
    fireEvent.click(screen.getByRole("button", { name: "判定" }));
    await waitFor(() => expect(mocks.explainDest).toHaveBeenCalled());
    // 结论渲染在 `.verdict__head`（页面描述里也有「命中」二字，不能用它当锚点）
    await waitFor(() =>
      expect(document.querySelector(".verdict__head")?.textContent ?? "").not.toBe(""),
    );
    return document.body.textContent ?? "";
  }

  it("命中且有规则名 ⇒ 现状文案（「命中规则「X」」）", async () => {
    const text = await explainWith({
      rule_index: 2,
      rule_tag: "google-to-us",
      outbound: "node-us",
      reasons: [],
      undecidable: [],
    });
    expect(text).toContain("命中规则「google-to-us」");
    expect(text).toContain("node-us");
  });

  it("**命中但没有规则名** ⇒ 说「命中第 N 条规则」，不得说「未命中任何规则」", async () => {
    const text = await explainWith({
      rule_index: 2,
      rule_tag: null,
      outbound: "node-us",
      reasons: [],
      undecidable: [],
    });
    expect(text).toContain("命中第 3 条规则");
    expect(text).toContain("没有给出规则名");
    expect(text).toContain("node-us");
    expect(text, "这条明明命中了，不许说未命中").not.toContain("未命中任何规则");
  });

  it("反例：真的没命中（`rule_index === null`）⇒ 才是「未命中任何规则」", async () => {
    const text = await explainWith({
      rule_index: null,
      rule_tag: null,
      outbound: "",
      reasons: ["未命中任何规则，将使用第一条出站（通常是节点）"],
      undecidable: [],
    });
    expect(text).toContain("未命中任何规则");
    expect(text).not.toContain("命中规则「");
  });
});
