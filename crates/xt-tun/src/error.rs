use thiserror::Error;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("系统调用失败: {op} ({source})")]
    Syscall {
        op: &'static str,
        #[source]
        source: std::io::Error,
    },

    #[error("命令执行失败: {program} {args:?} → 退出码 {code:?}, stderr: {stderr}")]
    Command {
        program: String,
        args: Vec<String>,
        code: Option<i32>,
        stderr: String,
    },

    #[error("命令输出无法解析: {what}（原始输出: {raw}）")]
    Parse { what: &'static str, raw: String },

    #[error("输入校验失败: {0}")]
    Invalid(String),

    #[error("找不到默认路由，无法确定物理出口（当前网络可能未连接）")]
    NoDefaultRoute,

    #[error("无法为设备 {device} 找到对应的网络服务名")]
    NoNetworkService { device: String },

    #[error("会话快照读写失败: {0}")]
    Snapshot(String),
}

impl Error {
    pub(crate) fn syscall(op: &'static str, source: std::io::Error) -> Self {
        Self::Syscall { op, source }
    }
}

impl From<std::io::Error> for Error {
    fn from(source: std::io::Error) -> Self {
        Self::Syscall { op: "io", source }
    }
}
