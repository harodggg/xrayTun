/**
 * task-167：规则 id 与预设/内部规则重名的防呆（用户报的 `duplicate ruleTag preset-private`）。
 *
 * # 两个判据（都不用前端手抄预设 id 清单）
 *
 * | 判据 | 真源 | 能证明什么 |
 * |---|---|---|
 * | `duplicatedRuleIds(tags)` | **运行中配置**的 ruleTag：后端唯一化后重复的会带 `#2`/`#3` | 「**确实**撞了」 |
 * | `appPrefixedIds(rules)` | id 是否用了 App 自己的前缀（`preset-`/`internal-`，约定来自 `routing/mod.rs` 与 `xray/config.rs`） | 「**可能**撞」 |
 *
 * 「确实撞了」这一支是硬证据，但它在用户真实场景里**可能没有**：他当时的预设是
 * 「自定义」⇒ 运行的配置里只有他自己的规则，看不到重复标记。所以「可能撞」这一支必须一起做，
 * 并且措辞是**条件句**（`如果目标预设里也存在同名规则…`），不假装已确认。
 *
 * # 与后端行为一致（不许夸大）
 *
 * 后端（`task-165`）会给重复的加确定性后缀 ⇒ **保存/切换不会导致起不来**。
 * 所以这里是 `role="alert"` 的**告警**（指名 + 后果 + 怎么办），不是硬拦。
 */
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  snapshot: vi.fn(),
  tailLogs: vi.fn(),
  saveSettings: vi.fn(),
  start: vi.fn(),
  stop: vi.fn(),
  routingTopology: vi.fn(),
}));

vi.mock("./ipc", () => ({
  api: {
    snapshot: mocks.snapshot,
    tailLogs: mocks.tailLogs,
    saveSettings: mocks.saveSettings,
    start: mocks.start,
    stop: mocks.stop,
    routingTopology: mocks.routingTopology,
  },
  errorText: (e: unknown) =>
    typeof e === "string" ? e : e instanceof Error ? e.message : String(e),
  parseRecovery: () => null,
  subscribe: () => () => {},
}));

import Routing, { appPrefixedIds, duplicatedRuleIds } from "./pages/Routing";
import { scenarioSnapshot } from "./previewSnapshot";
import { StoreProvider } from "./store";
import type { RoutingPreset, RoutingRule } from "./types";

const WHEN = {
  domains: [] as string[],
  ip: [] as string[],
  ports: [] as unknown[],
  source_ip: [] as string[],
  inbound_tags: [] as string[],
  network: "both" as const,
  process_names: [] as string[],
  protocols: [] as string[],
};

const rule = (id: string, name: string): RoutingRule => ({
  id,
  name,
  enabled: true,
  when: { ...WHEN },
  then: { kind: "direct" },
});

// ---------------------------------------------------------------------------
// 判据本体
// ---------------------------------------------------------------------------

describe("task-167 · 两个判据", () => {
  it("`duplicatedRuleIds`：只认 `<base>#<n>`（后端唯一化留下的标记）", () => {
    expect(
      duplicatedRuleIds(["preset-private", "preset-private#2", "preset-ads", "internal-api#3"]),
    ).toEqual(["preset-private", "internal-api"]);
  });

  it("`duplicatedRuleIds` 反例：没有 `#n` ⇒ 空（不许把普通 tag 当重名）", () => {
    expect(duplicatedRuleIds(["preset-private", "google-to-us", "internal-fallback"])).toEqual([]);
  });

  it("`appPrefixedIds`：只挑 App 自有前缀的 id（`preset-`/`internal-`）", () => {
    expect(
      appPrefixedIds([rule("preset-private", "a"), rule("mine", "b"), rule("internal-api", "c")]),
    ).toEqual(["preset-private", "internal-api"]);
  });
});

// ---------------------------------------------------------------------------
// 渲染：确实撞了 / 可能撞 / 都没撞
// ---------------------------------------------------------------------------

/**
 * 只找**本卡**那条 alert —— 页面上还有一条既有的
 * `presetShadowsCustom` 告警（预设遮蔽自定义规则），它同样是 `role="alert"`。
 */
const collisionAlert = () =>
  screen.queryAllByRole("alert").find((a) => (a.textContent ?? "").includes("重名了")) ?? null;

async function renderRouting(opts: {
  preset: RoutingPreset;
  rules: RoutingRule[];
  tags: string[] | null;
}) {
  const base = scenarioSnapshot();
  mocks.snapshot.mockResolvedValue({
    ...base,
    settings: { ...base.settings, routing_preset: opts.preset, custom_rules: opts.rules },
  } as never);
  mocks.tailLogs.mockResolvedValue([]);
  if (opts.tags === null) mocks.routingTopology.mockRejectedValue(new Error("核心未运行"));
  else {
    mocks.routingTopology.mockResolvedValue({
      ...base,
      rule: opts.tags.map((tag, index) => ({ index, tag, conditions: [], outbound: "direct" })),
    });
  }
  render(
    <StoreProvider>
      <Routing />
    </StoreProvider>,
  );
  await screen.findByRole("radiogroup", { name: "分流预设" });
}

describe("task-167 · 确实撞了 ⇒ 指名告警（且说清仍能启动）", () => {
  it("运行中配置出现 `preset-private#2` + 自定义规则里有 `preset-private` ⇒ alert 指名", async () => {
    await renderRouting({
      preset: "bypass_mainland",
      rules: [rule("preset-private", "我的直连规则")],
      tags: ["preset-private", "preset-private#2", "preset-ads"],
    });

    await waitFor(() => expect(collisionAlert()).not.toBeNull());
    const text = collisionAlert()!.textContent ?? "";
    expect(text).toContain("preset-private");
    expect(text).toContain("duplicate ruleTag");
    expect(text, "不许夸大：必须说清仍能启动").toContain("能启动");
    expect(text, "顺序要说清").toContain("预设那条在前");
    // 行上也要有标记
    expect(screen.getByText("与预设重名")).toBeTruthy();
  });

  it("反例：没有 `#n` 标记 ⇒ **不出现** alert（只有「可能」的 info 提示）", async () => {
    await renderRouting({
      preset: "bypass_mainland",
      rules: [rule("preset-private", "我的直连规则")],
      tags: ["preset-private", "preset-ads"],
    });
    await waitFor(() => expect(mocks.routingTopology).toHaveBeenCalled());
    expect(collisionAlert(), "没有 #n 标记就不是「确实撞了」").toBeNull();
    expect((await screen.findByRole("status")).textContent).toContain("preset-private");
  });

  it("反例：自定义 id 与预设无关 ⇒ 两种提示都不出现", async () => {
    await renderRouting({
      preset: "bypass_mainland",
      rules: [rule("google-to-us", "Google 走美国")],
      tags: ["preset-private", "preset-ads"],
    });
    await waitFor(() => expect(mocks.routingTopology).toHaveBeenCalled());
    expect(collisionAlert()).toBeNull();
    expect(screen.queryByText(/与预设重名|id 可能重名/)).toBeNull();
  });
});

describe("task-167 · 可能撞（用户真实场景：预设本是「自定义」）", () => {
  it("读不到运行中配置 ⇒ 条件句提示必须**如实说没确认**", async () => {
    await renderRouting({
      preset: "custom",
      rules: [rule("preset-private", "我的直连规则")],
      tags: null,
    });
    const hint = await screen.findByRole("status");
    const text = hint.textContent ?? "";
    expect(text).toContain("preset-private");
    expect(text).toContain("如果");
    expect(text, "读不到就要说读不到").toContain("无法确认");
    expect(collisionAlert()).toBeNull();
  });

  it("读到了配置、但没有重复标记 ⇒ 说明「当前没有发现重复标记」", async () => {
    await renderRouting({
      preset: "custom",
      rules: [rule("preset-private", "我的直连规则")],
      tags: ["preset-private", "internal-fallback"],
    });
    const hint = await screen.findByRole("status");
    expect(hint.textContent).toContain("没有发现重复标记");
  });

  it("**切预设前**就能看到提示（提示在预设区里，不是藏在规则区）", async () => {
    await renderRouting({
      preset: "custom",
      rules: [rule("preset-ads", "我的广告拦截")],
      tags: ["preset-ads"],
    });
    const hint = await screen.findByRole("status");
    const presetSection = document.querySelectorAll(".page__sec")[0]!;
    expect(presetSection.contains(hint), "提示必须挂在预设那一节里").toBe(true);
    // 点预设也不会崩（保存通路不变）
    fireEvent.click(screen.getByRole("radio", { name: /绕过大陆/ }));
    await waitFor(() => expect(mocks.saveSettings).toHaveBeenCalled());
  });
});
