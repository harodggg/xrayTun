/**
 * task-68 ①：`degraded`（探测失败、看门狗还没重建）必须**进 `appStatus`**。
 *
 * # 防的故障
 *
 * task-60 把自救提示渲染在 `App.tsx` / `Dashboard.tsx` 的可见元素上，但
 * **没进 `appStatus` 的 `detail`** —— 而顶栏 `.sr-only` + `role="status"` 的
 * live region 读的正是 `detail`。后果：那句「整机断网时先点『断开』」
 * **读屏用户完全收不到**，而那正是「用户不知道该断开」这个问题的修复。
 * 一个只有看得见的人才收到的自救提示，等于没修那个问题。
 *
 * # 同时守住的另一面
 *
 * `degraded` **不是** `busy`、**不是** `failed`：状态没变（`running` 仍为真），
 * 只是「可能正在变坏 + 有一个自救动作」。色调与状态词必须保持原样 ——
 * 把设计内的 10 秒窗口染成告警色是另一种假陈述（task-45/47 刚统一过语义）。
 */

import { describe, expect, it } from "vitest";

import { recoveryView } from "./ipc";
import { appStatus, DASH_TONE_CLASS } from "./topbarStatus";

const base = {
  mode: "tun" as const,
  running: true,
  routesCommitted: true,
  lastError: null as string | null,
  corePath: "/Applications/XrayTun.app/Contents/Resources/xray",
  socksPort: 10808,
  httpPort: 10809,
};

/** 造一个「探测已经失败 N 次、但还没开始重建」的恢复态。 */
const degraded = (failures: number) => recoveryView(
  {
    recovering: false,
    attempt: 0,
    probe_failures: failures,
    started_unix: null,
    last_outcome: null,
    finished_unix: null,
  },
  true,
);

const healthy = recoveryView(
  {
    recovering: false,
    attempt: 0,
    probe_failures: 0,
    started_unix: null,
    last_outcome: null,
    finished_unix: null,
  },
  true,
);

describe("degraded 折进 appStatus：live region 能读到自救句", () => {
  it("detail（= live region 的文本）必须包含「断开」自救提示", () => {
    const r = appStatus({ ...base, recovery: degraded(1) });
    expect(r.detail).toContain("断开");
    expect(r.detail).toContain("整台 Mac");
    // sub 是仪表盘那行；两处同一份文本，不能只在一处。
    expect(r.sub).toContain("断开");
  });

  it("**状态没变**：TUN 已接管时仍是 on + 「已连接」（不是 busy / failed）", () => {
    const healthyStatus = appStatus({ ...base, recovery: healthy });
    const r = appStatus({ ...base, recovery: degraded(1) });
    expect(r.tone).toBe(healthyStatus.tone);
    expect(r.tone).toBe("on");
    expect(r.label).toBe(healthyStatus.label);
    expect(r.label).toBe("已连接");
    expect(DASH_TONE_CLASS[r.tone]).toBe(DASH_TONE_CLASS.on);
  });

  it("探测成功（probe_failures=0）时**没有**自救提示 —— 健康态不许开始报警", () => {
    const r = appStatus({ ...base, recovery: healthy });
    expect(r.sub).toBeNull();
    expect(r.detail).not.toContain("断开");
    expect(r.detail).not.toContain("探测失败");
  });

  it("系统代理模式下也被折进 sub/detail（两句并存，不是互相覆盖）", () => {
    const r = appStatus({
      ...base,
      mode: "system_proxy",
      routesCommitted: false,
      recovery: degraded(1),
    });
    expect(r.tone).toBe("partial");
    expect(r.label).toBe("本地代理入口已就绪");
    // 系统代理那句「需要手动指向」仍在 —— 不能被自救句挤掉。
    expect(r.sub).toContain("系统代理未被本应用修改");
    expect(r.sub).toContain("127.0.0.1:10808");
    expect(r.sub).toContain("断开");
    expect(r.detail).toContain("需要手动");
    expect(r.detail).toContain("断开");
  });

  it("failed 的基础状态**不被覆盖**（自救提示不能把更严重的结论说轻）", () => {
    const r = appStatus({ ...base, corePath: null, recovery: degraded(1) });
    expect(r.tone).toBe("failed");
    expect(r.label).toBe("未找到核心");
    expect(r.detail).not.toContain("断开");
  });

  it("recovering 与 failed 的语义没有被 degraded 混进来（task-45/47 的语义不退化）", () => {
    const recovering = recoveryView(
      {
        recovering: true,
        attempt: 2,
        probe_failures: 2,
        started_unix: null,
        last_outcome: null,
        finished_unix: null,
      },
      false,
    );
    const rec = appStatus({ ...base, running: false, recovery: recovering });
    expect(rec.tone).toBe("busy");
    expect(rec.label).toContain("正在自动恢复");
    expect(rec.detail).not.toContain("先点「断开」");

    const failed = recoveryView(
      {
        recovering: false,
        attempt: 2,
        probe_failures: 0,
        started_unix: null,
        last_outcome: "direct_fallback",
        finished_unix: null,
      },
      false,
    );
    const fl = appStatus({ ...base, running: false, recovery: failed });
    expect(fl.tone).toBe("failed");
    expect(fl.label).toBe("自动恢复失败");
    expect(fl.detail).not.toContain("先点「断开」");
  });

  it("次数如实进 detail（1 次与 2 次是不同文本 → live region 各播一次，不重复刷）", () => {
    const one = appStatus({ ...base, recovery: degraded(1) });
    const two = appStatus({ ...base, recovery: degraded(2) });
    expect(one.detail).toContain("连续 1 次");
    expect(two.detail).toContain("连续 2 次");
    // 同一份输入必须得到**同一份文本** —— 否则 React 会因为字符串抖动反复改 DOM，
    // 读屏就会重复播报（详见报告里的 MutationObserver 实测）。
    const twice = appStatus({ ...base, recovery: degraded(2) });
    expect(twice.detail).toBe(two.detail);
  });
});

describe("系统代理模式：界面不再比事实强（同卡第二处）", () => {
  it("label 不声称「系统代理已启用」，并把「需要手动指向」第一次说给用户", () => {
    const r = appStatus({ ...base, mode: "system_proxy", recovery: healthy });
    expect(r.tone).toBe("partial");
    expect(r.label).toBe("本地代理入口已就绪");
    expect(r.label).not.toContain("已启用");
    expect(r.sub).toContain("系统代理未被本应用修改");
    expect(r.sub).toContain("需要手动");
    expect(r.detail).toContain("需要手动");
    // 端口来自快照；读得到就写具体值。
    expect(r.detail).toContain("127.0.0.1:10808");
  });

  it("端口读不到时一个数字都不写（红线：不编）", () => {
    const r = appStatus({
      ...base,
      mode: "system_proxy",
      socksPort: null,
      httpPort: null,
      recovery: healthy,
    });
    expect(`${r.label}${r.sub}${r.detail}`).not.toMatch(/127\.0\.0\.1:\d/);
    expect(r.detail).toContain("本地端口");
  });
});
