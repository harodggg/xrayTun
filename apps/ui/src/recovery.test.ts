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
