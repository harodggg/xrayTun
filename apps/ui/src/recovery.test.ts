/**
 * 自动恢复的三态渲染逻辑测试（task-22）。
 *
 * # 防的是哪个故障
 *
 * 用户最初的抱怨：「开机后网络应该自动连上，不需要点击连接」/「换网后要能自己恢复」。
 * 后端**确实**会自动重建隧道（看门狗），但界面把「正在恢复」显示成「未连接」，
 * 还留一个**可点的「连接」按钮** —— 用户看到就去点，正好和看门狗抢
 * （`core.rs` 注释提过启动会被多处并发调用）。
 *
 * 所以这里锁住的是**界面语义**，不是实现细节：
 * 1. `recovering` 时按钮**绝不能**是「连接」；
 * 2. 文案里永远有「恢复」，不会退化成「未连接」；
 * 3. 成功之后**不留残影**（不能一直显示「正在恢复」）；
 * 4. 失败（`direct_fallback`）要说清「已退回直连」并保持按钮可点；
 * 5. 没有结构化状态时**什么都不说** —— 不猜、不编。
 *
 * 这些都是纯函数，所以不必用 jsdom 渲染（渲染层由 CDP 在真浏览器里核对）。
 */

import { describe, expect, it } from "vitest";

import { parseRecovery, recoveryView } from "./ipc";
// 顶栏唯一主按钮的可点性（task-72：仪表盘那颗等价按钮已收敛掉，不变量搬到这里）。
import { runButtonDisabled } from "./topbarStatus";
import type { RecoveryState } from "./types";

function rec(over: Partial<RecoveryState> = {}): RecoveryState {
  return {
    recovering: false,
    attempt: 1,
    probe_failures: 0,
    started_unix: null,
    last_outcome: null,
    finished_unix: null,
    ...over,
  };
}

describe("自动恢复：界面三态", () => {
  it("正在恢复：按钮**不是**「连接」——这是本任务的核心不变量", () => {
    // 防的故障：恢复期间出现「未连接 + 可点的连接按钮」，用户点了和看门狗抢。
    const v = recoveryView(rec({ recovering: true, attempt: 2, probe_failures: 2 }), false);
    expect(v.phase).toBe("recovering");
    expect(v.button).not.toBe("connect");
    expect(v.button).toBe("recovering");
    expect(v.text).toContain("正在自动恢复");
    expect(v.text).toContain("第 2 次");
    // 文案里不能出现会被误读成「没连上」的词
    expect(v.text).not.toContain("未连接");
  });

  it("正在恢复：即使进程还报 running=true，也仍然按「恢复中」渲染", () => {
    // 防的故障：看门狗先 stop 再 start 之间有一瞬 running 还是 true，
    // 界面若按 running 优先就会闪成「断开」——状态仍以结构化状态为准。
    const v = recoveryView(rec({ recovering: true, attempt: 1 }), true);
    expect(v.phase).toBe("recovering");
    expect(v.button).toBe("recovering");
  });

  it("恢复成功：phase 回到 idle（**不残留**「正在恢复」），只给一次性提示", () => {
    // 防的故障（product-manager 实测）：后端重建成功时没清 last_notice，
    // 修好「该显示时不显示」会变成「恢复完了还一直显示正在恢复」。
    const v = recoveryView(
      rec({ recovering: false, attempt: 2, last_outcome: "recovered", finished_unix: 100 }),
      true,
    );
    expect(v.phase).toBe("idle");
    expect(v.text).toBeNull();
    expect(v.justRecovered).toBe(true);
    expect(v.button).toBe("disconnect");
  });

  it("恢复成功但进程没在跑：不给「已恢复」提示（不粉饰）", () => {
    const v = recoveryView(rec({ last_outcome: "recovered" }), false);
    expect(v.justRecovered).toBe(false);
    expect(v.phase).toBe("idle");
  });

  it("恢复失败（退回直连）：如实说失败 + 说清「流量不再走代理」+ 按钮可点", () => {
    // 防的故障：失败后只剩一个无事可做的禁用按钮，或把「退回直连」说成「断网」。
    const v = recoveryView(
      rec({ recovering: false, attempt: 3, last_outcome: "direct_fallback" }),
      false,
    );
    expect(v.phase).toBe("failed");
    expect(v.text).toContain("自动恢复失败");
    expect(v.text).toContain("直连");
    expect(v.text).toContain("不再走代理");
    expect(v.button).toBe("connect"); // 可操作：手动重连
  });

  it("没有恢复信息时什么都不说（不编「未在恢复」）", () => {
    // 防的故障：后端还没下发字段时前端反推一个「未在恢复」的结论。
    for (const missing of [null, undefined]) {
      const v = recoveryView(missing, true);
      expect(v.phase).toBe("idle");
      expect(v.text).toBeNull();
      expect(v.justRecovered).toBe(false);
    }
  });

  it("普通未连接：仍然是「连接」按钮（不要因为本任务把正常态也改坏）", () => {
    const v = recoveryView(null, false);
    expect(v.button).toBe("connect");
    expect(v.text).toBeNull();
  });
});

describe("载荷解析：畸形一律当「没有」", () => {
  it("正常载荷按字段读出", () => {
    const r = parseRecovery({
      recovery: {
        recovering: true,
        attempt: 4,
        probe_failures: 3,
        started_unix: 1700000000,
        last_outcome: null,
        finished_unix: null,
      },
    });
    expect(r).not.toBeNull();
    expect(r!.recovering).toBe(true);
    expect(r!.attempt).toBe(4);
    expect(r!.probe_failures).toBe(3);
    expect(r!.started_unix).toBe(1700000000);
  });

  it("runtime 不是对象 / 没有 recovery / recovery 是垃圾 → null", () => {
    expect(parseRecovery(null)).toBeNull();
    expect(parseRecovery("nope")).toBeNull();
    expect(parseRecovery({})).toBeNull();
    expect(parseRecovery({ recovery: 42 })).toBeNull();
    expect(parseRecovery({ recovery: "recovering" })).toBeNull();
  });

  it("结局名不认识 → 当作「还没有结局」，而不是猜一个", () => {
    const r = parseRecovery({ recovery: { recovering: false, attempt: 1, last_outcome: "whatever" } });
    expect(r!.last_outcome).toBeNull();
  });

  it("数值字段缺失 → 用 0（而不是 NaN 漏进界面）", () => {
    const r = parseRecovery({ recovery: { recovering: true } });
    expect(r!.attempt).toBe(0);
    expect(r!.probe_failures).toBe(0);
    expect(r!.started_unix).toBeNull();
  });
});

/**
 * task-60：**「可能正在变坏」必须看得见，但不能被说成「已经坏了」**。
 *
 * 防的故障（实测）：`probe_failures = 1` 时界面与健康时**逐字相同**
 * （绿点 + `已连接 · 香港 · 53 ms` + 按钮「断开」，无任何提示）。而看门狗是
 * 「连续 2 次失败才重建」，所以第 1 次失败之后的那 10 秒里用户正在断网，
 * 界面却说一切正常 —— 他会去查路由器/运营商/节点，**不会想到「先断开」**。
 *
 * 同一条测试同时守住反面：**不许把它做成错误态**（那会把设计内的过程说成故障）。
 */
describe("探测失败窗口（degraded）：可见，但不是错误态", () => {
  it("probe_failures=1 且核心在跑 → degraded，且**必须**带「断开」自救提示", () => {
    const v = recoveryView(rec({ probe_failures: 1 }), true);
    expect(v.phase).toBe("degraded");
    // 核心断言：自救提示必须出现，且点名那个动作。
    expect(v.hint).not.toBeNull();
    expect(v.hint).toContain("断开");
    expect(v.text).toContain("断开");
    expect(v.text).toContain("1 次");
    // 状态**没变**：连接仍在，按钮仍是「断开」而不是「连接」。
    expect(v.button).toBe("disconnect");
    expect(v.justRecovered).toBe(false);
  });

  it("probe_failures=0 → 什么都不说（不能因为加了这档就让健康态开始报警）", () => {
    const v = recoveryView(rec({ probe_failures: 0 }), true);
    expect(v.phase).toBe("idle");
    expect(v.hint).toBeNull();
    expect(v.text).toBeNull();
    expect(v.button).toBe("disconnect");
  });

  it("probe_failures=2 → 仍然是 degraded，且把次数如实写出来（不猜「即将重建」）", () => {
    // 后端在自增到阈值后**同一次迭代内**就 begin()，所以 `probe_failures=2`
    // 且未 recovering 是毫秒级的瞬态；这里只断言渲染是**如实**的，不编倒计时。
    const v = recoveryView(rec({ probe_failures: 2 }), true);
    expect(v.phase).toBe("degraded");
    expect(v.text).toContain("2 次");
  });

  it("核心没在跑时**不给**自救提示（次数是陈旧的，不能拿来吓用户）", () => {
    // 用户主动断开后，后端不会把 probe_failures 清零（只有探测成功时才清）。
    // 若这里仍然渲染，界面会在「用户自己关掉了」时显示「探测失败，先断开」——
    // 一句既无用又自相矛盾的话。
    const v = recoveryView(rec({ probe_failures: 1 }), false);
    expect(v.phase).toBe("idle");
    expect(v.hint).toBeNull();
    expect(v.button).toBe("connect");
  });

  it("恢复中**不给**自救提示（此刻按钮是禁用的，让用户去点「断开」就是自相矛盾）", () => {
    const v = recoveryView(rec({ recovering: true, attempt: 2, probe_failures: 2 }), false);
    expect(v.phase).toBe("recovering");
    expect(v.hint).toBeNull();
  });

  it("已退回直连时不给自救提示（那条路已由「自动恢复失败」说清）", () => {
    const v = recoveryView(rec({ probe_failures: 3, last_outcome: "direct_fallback" }), false);
    expect(v.phase).toBe("failed");
    expect(v.hint).toBeNull();
  });

  it("`hint` 只在 degraded 出现 —— 四种状态逐一锁住", () => {
    const cases: Array<[string, ReturnType<typeof recoveryView>]> = [
      ["recovering", recoveryView(rec({ recovering: true }), false)],
      ["failed", recoveryView(rec({ last_outcome: "direct_fallback" }), false)],
      ["degraded", recoveryView(rec({ probe_failures: 1 }), true)],
      ["idle", recoveryView(rec(), true)],
    ];
    for (const [name, v] of cases) {
      if (name === "degraded") expect(v.hint).not.toBeNull();
      else expect(v.hint, `${name} 不该有 hint`).toBeNull();
    }
  });
});

describe("③ 粘滞：手动重连成功后不得再显示「自动恢复失败」", () => {
  it("last_outcome 仍是 direct_fallback，但核心已经在跑 → 回到 idle（不再报失败）", () => {
    // 防的故障：后端不会在 `start_core` 成功时重置 last_outcome（core.rs 只写
    // begin / succeeded / fell_back / set_probe_failures），若前端不看 `running`，
    // 用户手动重连成功之后界面会显示红色「自动恢复失败 / 已退回直连」，
    // 而实际上隧道已经好了、流量也在走代理 —— 陈述与事实相反。
    const v = recoveryView(rec({ attempt: 2, last_outcome: "direct_fallback" }), true);
    expect(v.phase).toBe("idle");
    expect(v.text).toBeNull();
    expect(v.text ?? "").not.toContain("自动恢复失败");
    expect(v.button).toBe("disconnect");
  });

  it("同一份状态、核心没在跑 → 仍然如实报「已退回直连」（别把真失败也藏了）", () => {
    const v = recoveryView(rec({ attempt: 2, last_outcome: "direct_fallback" }), false);
    expect(v.phase).toBe("failed");
    expect(v.text).toContain("已退回直连");
    expect(v.button).toBe("connect");
  });

  it("重连成功后再失败一次探测 → 回到 degraded（不是 failed，也不是沉默）", () => {
    const v = recoveryView(
      rec({ attempt: 2, last_outcome: "direct_fallback", probe_failures: 1 }),
      true,
    );
    expect(v.phase).toBe("degraded");
    expect(v.hint).toContain("断开");
  });
});

/**
 * task-60 ②：仪表盘的「连接」按钮在恢复期间必须禁用。
 *
 * 防的故障（实测）：`?recovery=recovering` 时顶栏按钮是「正在恢复…」且禁用，
 * 而仪表盘同一屏给了一个**可点的「连接」**，它自己的副文案还写着「点了会打断它」。
 */
/**
 * task-60 ②（task-72 后搬到顶栏）：自动恢复期间，唯一的「连接/断开」按钮必须禁用。
 *
 * 防的故障（实测）：`?recovery=recovering` 时顶栏按钮是「正在恢复…」且禁用，
 * 而仪表盘同一屏曾给了一个**可点的「连接」**，它自己的副文案还写着「点了会打断它」。
 * task-72 把仪表盘那颗等价按钮收敛掉了（同一命令），所以这条不变量现在钉在
 * `runButtonDisabled()` 上 —— 它驱动**顶栏那唯一的一颗**。
 */
describe("顶栏唯一的主按钮：恢复期间必须禁用", () => {
  const opts = { runBusy: false, mode: "tun" as const };

  it("recovering → disabled（这条不变量不能随仪表盘那颗按钮一起消失）", () => {
    const rv = recoveryView(rec({ recovering: true, attempt: 2 }), false);
    expect(runButtonDisabled(rv, opts)).toBe(true);
  });

  it("idle → 可点（未连接时是「连接」，已连接时是「断开」）", () => {
    expect(runButtonDisabled(recoveryView(rec(), false), opts)).toBe(false);
    expect(runButtonDisabled(recoveryView(rec(), true), opts)).toBe(false);
  });

  it("failed → 可点（手动重连是出路）", () => {
    const rv = recoveryView(rec({ last_outcome: "direct_fallback" }), false);
    expect(runButtonDisabled(rv, opts)).toBe(false);
  });

  it("忙碌 / 直连模式 → 沿用原来的禁用条件（不要改坏）", () => {
    const idle = recoveryView(rec(), false);
    expect(runButtonDisabled(idle, { ...opts, runBusy: true })).toBe(true);
    expect(runButtonDisabled(idle, { ...opts, mode: "direct" })).toBe(true);
    expect(runButtonDisabled(recoveryView(rec(), true), { ...opts, mode: "direct" })).toBe(true);
  });
});
