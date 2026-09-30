//! xt-cli 的二进制入口：解析参数，跑 [`xt_cli::run`]，用它的返回值做退出码。
//!
//! 退出码的约定（脚本据此判断，别在别处另立一套）：
//! * 0 = 成功；
//! * 1 = daemon / IO / 契约层面的真实失败；
//! * 2 = 用法错误或本版本不支持的能力。

use clap::Parser;

#[tokio::main]
async fn main() {
    let cli = xt_cli::Cli::parse();
    std::process::exit(xt_cli::run(cli).await);
}
