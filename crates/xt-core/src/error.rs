use thiserror::Error;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("订阅内容为空或无法识别格式")]
    EmptySubscription,

    #[error("base64 解码失败: {0}")]
    Base64(String),

    #[error("第 {line} 行不是受支持的分享链接: {line_text}")]
    BadShareLink { line: usize, line_text: String },

    #[error("分享链接缺少必需字段: {0}")]
    MissingField(&'static str),

    /// 字段存在但取值不受支持。
    ///
    /// 之所以要单独一个变体而不是复用 `MissingField`：把**实际收到的值**
    /// 打进错误信息里，是排查「订阅导入后某些节点静默消失」的唯一线索。
    #[error("{field} 的取值不受支持: {value}（仅支持 {supported}）")]
    UnsupportedValue {
        field: &'static str,
        value: String,
        supported: &'static str,
    },

    #[error("非法 URL: {0}")]
    Url(#[from] url::ParseError),

    #[error("YAML 解析失败: {0}")]
    Yaml(String),

    #[error("JSON 处理失败: {0}")]
    Json(#[from] serde_json::Error),

    #[error("IO 失败: {0}")]
    Io(#[from] std::io::Error),

    #[error("找不到 Xray 核心可执行文件（尝试过: {0}）")]
    CoreNotFound(String),

    #[error("Xray 核心启动失败: {0}")]
    CoreSpawn(String),

    #[error("Xray 核心在 {0:?} 内未就绪")]
    CoreNotReady(std::time::Duration),

    #[error("Xray 配置非法: {0}")]
    InvalidConfig(String),

    #[error("探针失败: {0}")]
    Probe(String),

    #[error("读取核心流量统计失败: {0}")]
    Stats(String),

    #[error("配置读写失败: {0}")]
    Store(String),
}
