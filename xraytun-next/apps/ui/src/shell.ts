// xraytun-next · 壳能力（Tauri 侧）的唯一入口
//
// 为什么把「壳能做什么」收在这一个文件里：界面组件不该直接碰 invoke。
//  * 无头环境（活体验收 / 浏览器 / 单元测试）根本没有 Tauri 桥，静态 import 会在
//    **加载期**就炸掉整个页面；这里所有调用都走**动态 import**，只有真的按下按钮
//    那一刻才会去加载桥。
//  * 命令名与返回形状只在这里出现一次，界面只认这里导出的函数与类型。
//
// 壳侧固定的接口（apps/desktop/src/helper_install.rs，字段是 Rust 的 snake_case，
// serde 原样序列化）：
//   helper_status → HelperStatus
//   install_helper / uninstall_helper / restart_helper → 无返回值
// 失败时 reject 的值是**字符串**（Rust 的 `Result<(), String>`），不是 Error 对象；
// 这里不做包装、不改写，让调用方按原样展示给用户。

export interface HelperStatus {
  installed: boolean;
  plist_present: boolean;
  socket_present: boolean;
  reachable: boolean;
  binary_in_bundle: boolean;
}

/**
 * 壳启动 daemon 的结论（`daemon_launch::LaunchReport` 的逐字镜像）。
 *
 * `outcome` 的四个值是壳里的真实分支，不是这里另起的名字：
 * `already_listening` / `spawned` / `not_listening` / `unavailable`。
 */
export interface LaunchReport {
  outcome: 'already_listening' | 'spawned' | 'not_listening' | 'unavailable';
  /** 人话的原因；`already_listening` / `spawned` 时没有坏消息，是 `null`。 */
  reason: string | null;
  pid: number | null;
  bin: string | null;
  socket: string;
  /** daemon 的 stdout/stderr 落在这里 —— 连不上时该看的就是它。 */
  log_path: string | null;
}

/** 这个壳能不能装特权助手（由 App 启动时注入；开发 / 无头环境下为 false）。 */
export function canInstallHelper(): boolean {
  return typeof window !== 'undefined' && window.__XT_HELPER_INSTALL__ === true;
}

async function invokeShell<T>(command: string): Promise<T> {
  const { invoke } = await import('@tauri-apps/api/core');
  return invoke<T>(command);
}

export async function helperStatus(): Promise<HelperStatus> {
  return invokeShell<HelperStatus>('helper_status');
}

export async function installHelper(): Promise<void> {
  await invokeShell<null>('install_helper');
}

export async function uninstallHelper(): Promise<void> {
  await invokeShell<null>('uninstall_helper');
}

export async function restartHelper(): Promise<void> {
  await invokeShell<null>('restart_helper');
}

/** 这个壳能不能报 daemon 启动结论（只有 Tauri 壳注入；无头环境为 false）。 */
export function canReadLaunchReport(): boolean {
  return typeof window !== 'undefined' && window.__XT_LAUNCH_REPORT__ === true;
}

/**
 * 取壳的启动结论。取不到就 `null`：界面**不回落到猜测**，只是这一段不显示。
 * （命令缺失、壳还没写过报告都会落到这里 —— 两者都不是「启动失败」，不许当成失败说。）
 */
export async function daemonLaunchReport(): Promise<LaunchReport | null> {
  try {
    return await invokeShell<LaunchReport | null>('daemon_launch_report');
  } catch {
    return null;
  }
}
