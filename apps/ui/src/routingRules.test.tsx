/**
 * 路由规则编辑器（task-117）。
 *
 * # 为什么需要它
 *
 * 在它之前，「让某类流量走某个具体节点」（例如 Gemini 走美国、其余走香港）**只能手改
 * `settings.json`** —— 界面只展示规则，还明说「请改 custom_rules 字段」。
 * 而数据模型一直支持：`RuleAction::Proxy { outbound: Some(tag) }` 会直接变成 Xray 的
 * `outboundTag`，`build_outbounds` 给每个节点都建了出站（tag = `node-<id>`）。
 *
 * # 这一组钉住什么
 *
 * 1. **往返**：已有规则 → 界面显示成正确的控件值（反序列化）；编辑 → 保存载荷里的
 *    `custom_rules` 结构正确（序列化）。`outbound: null`（当前节点）与
 *    `outbound: "node-<id>"`（指定节点）**两种都要**；
 * 2. **顺序即优先级**：界面序号 = 数组顺序 = 保存顺序；↑/↓ 改的是**数组顺序**，
 *    而 Xray 自上而下取第一条命中（`xray/config.rs` 的 `merge_rules` 把预设放前面、
 *    自定义规则按数组顺序追加）；
 * 3. **预设遮蔽必须显式警告**（不是静默无效）：保留预设 + 有自定义规则 ⇒ 警告里要出现
 *    「排在预设之后」与具体例子 `geosite:google`；预设为 `custom` 或没有自定义规则 ⇒ **不出现**；
 * 4. **指向已删节点的规则**要给出可读提示，而不是让人以为它生效了。
 */
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({
  snapshot: vi.fn(),
  tailLogs: vi.fn(),
  saveSettings: vi.fn(),
  start: vi.fn(),
  stop: vi.fn(),
}));

vi.mock("./ipc", () => ({
  api: {
    snapshot: mocks.snapshot,
    tailLogs: mocks.tailLogs,
    saveSettings: mocks.saveSettings,
    start: mocks.start,
    stop: mocks.stop,
  },
  errorText: (e: unknown) =>
    typeof e === "string" ? e : e instanceof Error ? e.message : String(e),
  parseRecovery: () => null,
  subscribe: () => () => {},
}));

import Routing from "./pages/Routing";
import { scenarioSnapshot } from "./previewSnapshot";
import { StoreProvider } from "./store";
import type { RoutingPreset, RoutingRule } from "./types";

const EMPTY_WHEN = {
  domains: [] as string[],
  ip: [] as string[],
  ports: [] as unknown[],
  source_ip: [] as string[],
  inbound_tags: [] as string[],
  network: "both" as const,
  process_names: [] as string[],
  protocols: [] as string[],
};

function rule(over: Partial<RoutingRule> & { id: string; name: string }): RoutingRule {
  return { enabled: true, when: { ...EMPTY_WHEN }, then: { kind: "proxy", outbound: null }, ...over };
}

/** 造快照：预览快照自带 4 个节点（n-hk-1 / n-jp-2 / n-us-3 / n-sg-4）与它们的延迟。 */
function snap(preset: RoutingPreset, rules: RoutingRule[]) {
  const base = scenarioSnapshot();
  return {
    ...base,
    settings: { ...base.settings, routing_preset: preset, custom_rules: rules },
  } as never;
}

async function renderRouting(preset: RoutingPreset, rules: RoutingRule[]) {
  mocks.snapshot.mockResolvedValue(snap(preset, rules));
  mocks.tailLogs.mockResolvedValue([]);
  // 后端「原样保存」并回快照
  mocks.saveSettings.mockImplementation(async (s: unknown) => ({ ...(snap(preset, []) as object), settings: s }));
  const r = render(
    <StoreProvider>
      <Routing />
    </StoreProvider>,
  );
  await screen.findByRole("radiogroup", { name: "分流预设" });
  return r;
}

/** 规则行（用规则名定位；同一时刻只展开一条，所以「编辑」按钮按行取）。 */
const rowOf = (name: string) => screen.getByText(name).closest(".list__row") as HTMLElement;

/** 保存并取回载荷里的 custom_rules。 */
async function saveAndGetRules(): Promise<RoutingRule[]> {
  fireEvent.click(screen.getByRole("button", { name: "保存规则" }));
  await waitFor(() => expect(mocks.saveSettings).toHaveBeenCalledTimes(1));
  const payload = mocks.saveSettings.mock.calls[0]![0] as { custom_rules: RoutingRule[] };
  return payload.custom_rules;
}

/**
 * **真实生产数据**：本机 `settings.json` 里那 5 条 `custom_rules`（逐字抄下来，
 * 只去掉与本测试无关的导入/凭据）。存在本条的目的：证明「编辑器能不能原样处理
 * 用户机器上那条 `google-to-us`」，而不是只处理我编出来的形状。
 *
 * 配套的真实 `runtime/config.json` 里这条是：
 * `{"domain":["geosite:google","googleapis.com","gstatic.com","googleusercontent.com"],
 *   "outboundTag":"node-nd97712f6aa0fa28a","ruleTag":"google-to-us","type":"field"}`
 * ⇒ `ruleTag` 就是规则 `id`，`outboundTag` 就是 `then.outbound` 原样。
 */
const REAL_RULES: RoutingRule[] = [
  {
    id: "preset-private",
    name: "私有与保留地址直连",
    enabled: true,
    when: { ...EMPTY_WHEN, domains: ["geosite:private"], ip: ["geoip:private"] },
    then: { kind: "direct" },
  },
  {
    id: "preset-ads",
    name: "拦截常见广告域名",
    enabled: true,
    when: { ...EMPTY_WHEN, domains: ["geosite:category-ads-all"] },
    then: { kind: "block" },
  },
  {
    id: "google-to-us",
    name: "Google 系（含 Gemini）走美国",
    enabled: true,
    when: {
      ...EMPTY_WHEN,
      domains: ["geosite:google", "googleapis.com", "gstatic.com", "googleusercontent.com"],
    },
    then: { kind: "proxy", outbound: "node-nd97712f6aa0fa28a" },
  },
  {
    id: "preset-cn-domain",
    name: "大陆域名直连",
    enabled: true,
    when: { ...EMPTY_WHEN, domains: ["geosite:cn"] },
    then: { kind: "direct" },
  },
  {
    id: "preset-cn-ip",
    name: "大陆 IP 直连",
    enabled: true,
    when: { ...EMPTY_WHEN, ip: ["geoip:cn"] },
    then: { kind: "direct" },
  },
];

/** 本机 `nodes.json` 的两个真实节点（id / 名字逐字抄；地址用于区分，无凭据）。 */
const REAL_NODES = [
  { id: "n1d232c6b8c7a5004", name: "Xray-45.207.197.185" },
  { id: "nd97712f6aa0fa28a", name: "XrayTun-US" },
];

/** 用真实节点列表 + 真实规则渲染。 */
async function renderReal() {
  const base = scenarioSnapshot();
  const s = {
    ...base,
    nodes: REAL_NODES.map((n, i) => ({ ...base.nodes[i]!, ...n })),
    latency: {
      n1d232c6b8c7a5004: { ...base.latency["n-hk-1"]!, node_id: "n1d232c6b8c7a5004", server_rtt_ms: 46 },
      nd97712f6aa0fa28a: { ...base.latency["n-us-3"]!, node_id: "nd97712f6aa0fa28a", server_rtt_ms: 168 },
    },
    settings: { ...base.settings, routing_preset: "custom" as const, custom_rules: REAL_RULES },
  };
  mocks.snapshot.mockResolvedValue(s as never);
  mocks.tailLogs.mockResolvedValue([]);
  mocks.saveSettings.mockImplementation(async (ns: unknown) => ({ ...s, settings: ns }));
  render(
    <StoreProvider>
      <Routing />
    </StoreProvider>,
  );
  await screen.findByRole("radiogroup", { name: "分流预设" });
}

beforeEach(() => {
  vi.clearAllMocks();
  mocks.start.mockResolvedValue(snap("custom", []));
  mocks.stop.mockResolvedValue(snap("custom", []));
});

describe("路由规则编辑器（task-117）", () => {
  it("往返（指定节点）：已有规则显示成正确的控件值，编辑后再保存结构不变", async () => {
    const gemini = rule({
      id: "r1",
      name: "Gemini 走美国",
      when: { ...EMPTY_WHEN, domains: ["gemini.google.com", "aistudio.google.com"] },
      then: { kind: "proxy", outbound: "node-n-us-3" },
    });
    await renderRouting("custom", [gemini]);

    // 反序列化：行上显示的是**节点名**（不是 node-xxx 这种 tag），且没有失效提示
    expect(within(rowOf("Gemini 走美国")).getByText(/代理（/)).toBeTruthy();
    expect(screen.queryByText(/已经不在节点列表里/)).toBeNull();

    // 展开编辑：域名与动作/节点都应当是这条规则的当前值
    fireEvent.click(within(rowOf("Gemini 走美国")).getByRole("button", { name: "编辑" }));
    const domains = screen.getByDisplayValue("gemini.google.com, aistudio.google.com");
    expect(domains).toBeTruthy();
    expect((screen.getByRole("combobox", { name: /动作/ }) as HTMLSelectElement).value).toBe(
      "proxy_node",
    );
    const nodeSelect = screen.getByRole("combobox", { name: /指定节点/ }) as HTMLSelectElement;
    expect(nodeSelect.value).toBe("node-n-us-3");

    // 改一下延迟无关的东西再保存 → 结构仍是带 outbound 的 proxy
    fireEvent.change(screen.getByDisplayValue("Gemini 走美国"), { target: { value: "Gemini→美国" } });
    const saved = await saveAndGetRules();
    expect(saved).toHaveLength(1);
    expect(saved[0]!.name).toBe("Gemini→美国");
    expect(saved[0]!.id).toBe("r1");
    expect(saved[0]!.then).toEqual({ kind: "proxy", outbound: "node-n-us-3" });
    expect(saved[0]!.when.domains).toEqual(["gemini.google.com", "aistudio.google.com"]);
    // 其余条件字段必须原样保留（别被编辑器悄悄清空）
    expect(Object.keys(saved[0]!.when).sort()).toEqual(Object.keys(EMPTY_WHEN).sort());
  });

  it("往返（当前节点）：`outbound: null` 不会被写成具体节点", async () => {
    await renderRouting("custom", [
      rule({ id: "r2", name: "全部走当前节点", when: { ...EMPTY_WHEN, domains: ["geosite:cn"] } }),
    ]);
    fireEvent.click(within(rowOf("全部走当前节点")).getByRole("button", { name: "编辑" }));

    const action = screen.getByRole("combobox", { name: /动作/ }) as HTMLSelectElement;
    expect(action.value).toBe("proxy_current");

    // 注意：「保存规则」在没有草稿改动时是**禁用**的（没改就没得存），所以先真的改一个字段。
    fireEvent.change(screen.getByDisplayValue("全部走当前节点"), { target: { value: "走当前节点" } });
    const saved = await saveAndGetRules();
    expect(saved[0]!.then).toEqual({ kind: "proxy", outbound: null });
  });

  it("顺序即优先级：界面序号=数组顺序；↑ 之后保存的顺序真的换了", async () => {
    const a = rule({ id: "r1", name: "第一条", when: { ...EMPTY_WHEN, domains: ["geosite:google"] } });
    const b = rule({ id: "r2", name: "第二条", then: { kind: "direct" } });
    await renderRouting("custom", [a, b]);

    // 序号按数组顺序显示（1、2），且顺序与数组一致
    const names = () =>
      [...document.querySelectorAll(".list__row .list__name")].map((e) => e.textContent);
    expect(names()[0]).toContain("第一条");
    expect(names()[1]).toContain("第二条");

    // 把第二条上移 → 它成为第一条（= 优先级更高）
    fireEvent.click(within(rowOf("第二条")).getByRole("button", { name: "↑" }));
    expect(names()[0]).toContain("第二条");

    const saved = await saveAndGetRules();
    expect(
      saved.map((r) => r.id),
      "保存顺序必须与界面顺序一致 —— 它是 Xray 的优先级（自上而下第一条命中）",
    ).toEqual(["r2", "r1"]);
  });

  it("顺序边界：第一条不能上移、最后一条不能下移", async () => {
    await renderRouting("custom", [rule({ id: "r1", name: "唯一一条" })]);
    fireEvent.click(within(rowOf("唯一一条")).getByRole("button", { name: "编辑" }));
    expect(screen.getByRole("button", { name: "↑" })).toHaveProperty("disabled", true);
    expect(screen.getByRole("button", { name: "↓" })).toHaveProperty("disabled", true);
  });

  it("预设遮蔽：保留预设 + 有自定义规则 ⇒ **显式警告**（含 geosite:google 这个具体例子）", async () => {
    await renderRouting("bypass_mainland", [
      rule({ id: "r1", name: "Google 走美国", when: { ...EMPTY_WHEN, domains: ["geosite:google"] } }),
    ]);

    const warn = screen.getByRole("alert");
    const text = warn.textContent ?? "";
    expect(text, "必须说清「排在预设之后」").toContain("排在预设之后");
    expect(text, "必须给出具体例子，否则用户仍然不知道哪些域名受影响").toContain("geosite:google");
    expect(text, "必须给出可执行的下一步").toContain("自定义");
  });

  it("反例：预设为「自定义」时不警告（此时自定义规则就是全部规则）", async () => {
    await renderRouting("custom", [rule({ id: "r1", name: "X" })]);
    expect(screen.queryByText(/排在预设之后/)).toBeNull();
  });

  it("反例：保留预设但没有自定义规则时不警告（没有东西会被遮蔽）", async () => {
    await renderRouting("bypass_mainland", []);
    expect(screen.queryByText(/排在预设之后/)).toBeNull();
  });

  it("指向已删节点的规则：给出可读提示，而不是让人以为它生效了", async () => {
    await renderRouting("custom", [
      rule({
        id: "r1",
        name: "指向已删节点",
        then: { kind: "proxy", outbound: "node-已删除" },
      }),
    ]);
    expect(
      screen.getAllByText(/已经不在节点列表里/).length,
      "节点被删掉后，规则指向的出站失效必须说出来",
    ).toBeGreaterThan(0);
    expect(screen.getByText("node-已删除")).toBeTruthy(); // 指名到底是哪个 tag
  });

  it("新增规则：默认是「代理（当前选中的节点）」这种最不容易出错的档", async () => {
    await renderRouting("custom", []);
    fireEvent.click(screen.getByRole("button", { name: "新增规则" }));
    const action = screen.getByRole("combobox", { name: /动作/ }) as HTMLSelectElement;
    expect(action.value).toBe("proxy_current");
    const saved = await saveAndGetRules();
    expect(saved).toHaveLength(1);
    expect(saved[0]!.then).toEqual({ kind: "proxy", outbound: null });
  });

  /**
   * **用用户机器上的真数据**跑一遍：这 5 条 `custom_rules` 就是本机
   * `settings.json` 的内容，而它们生成的本机 `runtime/config.json` 里
   * `rules[2] = {domain:[geosite:google, ...], outboundTag:"node-nd97712f6aa0fa28a",
   * ruleTag:"google-to-us"}`（原文已核对）。
   *
   * 所以这条测的不是「我编的形状能不能过」，而是「编辑器会不会把用户真在用的
   * 那份规则改坏」—— 顺序、id、when 的每个字段、outbound 的 tag 都要逐字保留。
   */
  it("真实生产规则往返：本机 settings.json 的 5 条规则过了编辑器仍逐字一致", async () => {
    await renderReal();

    // 行上显示的是**节点名**（XrayTun-US），不是 node-nd977… 这个 tag
    const row = rowOf("Google 系（含 Gemini）走美国");
    expect(within(row).getByText(/代理（XrayTun-US）/)).toBeTruthy();
    expect(screen.queryByText(/已经不在节点列表里/)).toBeNull();

    fireEvent.click(within(row).getByRole("button", { name: "编辑" }));
    expect((screen.getByRole("combobox", { name: /动作/ }) as HTMLSelectElement).value).toBe(
      "proxy_node",
    );
    const nodeSelect = screen.getByRole("combobox", { name: /指定节点/ }) as HTMLSelectElement;
    expect(nodeSelect.value).toBe("node-nd97712f6aa0fa28a");
    // 下拉里带延迟（延迟取自 snapshot.latency[].server_rtt_ms）
    expect(within(nodeSelect).getByRole("option", { name: "XrayTun-US · 168ms" })).toBeTruthy();

    // 只改名字，保存后其余四条与这条的其余字段必须逐字不动、顺序不动
    fireEvent.change(screen.getByDisplayValue("Google 系（含 Gemini）走美国"), {
      target: { value: "Google 系（含 Gemini）走美国节点" },
    });
    const saved = await saveAndGetRules();
    expect(saved.map((r) => r.id)).toEqual([
      "preset-private",
      "preset-ads",
      "google-to-us",
      "preset-cn-domain",
      "preset-cn-ip",
    ]);
    expect(saved[2]).toEqual({ ...REAL_RULES[2]!, name: "Google 系（含 Gemini）走美国节点" });
    expect(saved.filter((_, i) => i !== 2)).toEqual(REAL_RULES.filter((_, i) => i !== 2));
    expect(saved).toHaveLength(REAL_RULES.length);
  });
});
