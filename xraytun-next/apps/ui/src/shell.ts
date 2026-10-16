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
