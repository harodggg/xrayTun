//! 响应体裁剪：**改 body 就必须同时改长度**。
//!
//! # 这一条为什么单独立一个模块
//!
//! 设计文档把它列为「必测项」：`strip_json` 必须重算 `Content-Length`，
//! **不许出现"内容剪了但长度没改"** —— 那会让客户端读到截断/挂起的响应，
//! 而症状（页面白屏、接口 500）离根因（我们少写了一个头）非常远。
//!
//! 所以这里的 API **没有**"只改 body"的入口：唯一改 body 的函数是
//! [`apply_body_change`]，它顺带把头改成一致的状态。
//!
//! # `chunked` 怎么办
//!
//! 代理侧会**先把响应体读全**（带上限）再判定 —— 已经被我们完整持有的 body，
//! 继续用 chunked 没有任何意义，所以 [`apply_body_change`] 会把
//! `Transfer-Encoding: chunked` **去掉**并写上精确的 `Content-Length`。
//! 这也是唯一不会"长度与内容不一致"的做法。
//!
//! # 没改就不动它
//!
//! [`strip_json_array_entries`] 在**没有删掉任何条目**时返回 `Ok(None)`：
//! 调用方应当把原始字节原样转发（连 framing 都不动）。
//! 「改了个一模一样的东西」也会带来风险，没有必要。

use serde_json::Value;

use crate::http1::{remove_header, set_header};

/// 裁剪过程中的错误。**每一种都要能被如实报出来**，不许猜。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RewriteError {
    /// body 不是合法 JSON。
    NotJson(String),
    /// JSON Pointer 没指到一个数组。
    PathNotArray(String),
    /// 重新序列化失败（理论上不会发生，但不 panic）。
    Serialize(String),
}

impl std::fmt::Display for RewriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotJson(e) => write!(f, "响应体不是 JSON：{e}"),
            Self::PathNotArray(p) => write!(f, "JSON 指针 {p:?} 没指到数组"),
            Self::Serialize(e) => write!(f, "重新序列化失败：{e}"),
        }
    }
}

impl std::error::Error for RewriteError {}

/// 删掉 `pointer` 所指数组里满足 `remove` 的条目。
///
/// * `pointer` 是 RFC 6901 的 JSON Pointer：`""` = 根（根必须是数组）、
///   `"/data/items"` = 嵌套路径；
/// * **没有任何条目被删** ⇒ `Ok(None)`（调用方原样转发，别动 framing）；
/// * 删了 ⇒ `Ok(Some(新字节))`。
pub fn strip_json_array_entries(
    body: &[u8],
    pointer: &str,
    remove: impl Fn(&Value) -> bool,
) -> Result<Option<Vec<u8>>, RewriteError> {
    let mut root: Value =
        serde_json::from_slice(body).map_err(|e| RewriteError::NotJson(e.to_string()))?;

    let target = if pointer.is_empty() {
        &mut root
    } else {
        root.pointer_mut(pointer)
            .ok_or_else(|| RewriteError::PathNotArray(pointer.to_string()))?
    };
    let Some(arr) = target.as_array_mut() else {
        return Err(RewriteError::PathNotArray(pointer.to_string()));
    };

    let before = arr.len();
    arr.retain(|item| !remove(item));
    if arr.len() == before {
        return Ok(None);
    }
    let bytes = serde_json::to_vec(&root).map_err(|e| RewriteError::Serialize(e.to_string()))?;
    Ok(Some(bytes))
}

/// 把新的 body 写回头里，**并保证长度与内容一致**。
///
/// 做三件事：
/// 1. `Content-Length` 设成新 body 的精确长度（没有就加上）；
/// 2. 去掉 `Transfer-Encoding`（body 已经被我们完整持有，chunked 没有意义了）；
/// 3. 去掉 `Content-Encoding`？—— **不**。见下面的注意。
///
/// # 注意：这里只处理**未压缩**的 body
///
/// 调用方必须在读取时就处理内容编码（例如请求 `Accept-Encoding: identity`，
/// 或自己解压）。**压缩过的 body 不许传进来** —— 那时 `Content-Length` 指的是
/// 压缩后的长度，而裁剪是在解压后的字节上做的，两者混在一起又是一个
/// "长度与内容不一致"的坑。所以这个函数只断言长度一致，不试图猜编码。
pub fn apply_body_change(
    headers: &mut Vec<(String, String)>,
    new_body: &[u8],
) -> Result<(), RewriteError> {
    remove_header(headers, "Transfer-Encoding");
    set_header(headers, "Content-Length", &new_body.len().to_string());
    debug_assert_eq!(
        crate::http1::get_header(headers, "Content-Length")
            .and_then(|v| v.parse::<usize>().ok()),
        Some(new_body.len()),
        "改完 body 之后 Content-Length 必须等于新长度"
    );
    Ok(())
}

/// 头与 body 交给调用方之前最后一道自检：长度必须对得上。
///
/// 放在这里而不是散在各处：**每一次真正发出响应之前都该过一遍**。
pub fn length_matches(headers: &[(String, String)], body: &[u8]) -> Result<(), RewriteError> {
    let declared = crate::http1::get_header(headers, "Content-Length")
        .and_then(|v| v.trim().parse::<usize>().ok());
    match declared {
        Some(n) if n == body.len() => Ok(()),
        Some(n) => Err(RewriteError::NotJson(format!(
            "Content-Length 声明 {n}，实际 body {} —— 拒绝发出（那会把客户端搞崩）",
            body.len()
        ))),
        // 没有 Content-Length 时不能判定（连接关闭分帧）。调用方应当先写上。
        None => Err(RewriteError::NotJson(
            "响应缺少 Content-Length，无法自检长度一致性".into(),
        )),
    }
}

// ---------------------------------------------------------------------------
// 裁剪接缝：谁来裁、为什么没裁
// ---------------------------------------------------------------------------

/// "为什么没改"的**显式**原因。
///
/// 每一次没改都要能被计数与解释：静默地"什么都没发生"是这层最难查的失败，
/// 因为没有 symptom 可看（响应是对的，只是广告还在）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclineReason {
    /// 没有配置裁剪（[`crate::serve`] 的默认路径，接缝存在但不启用）。
    NotConfigured,
    /// 响应 `Content-Type` 不是 JSON 类型。
    NotJsonContentType,
    /// body 超过 [`MAX_REWRITE_BYTES`]：**不裁** —— 否则要为一个响应缓冲任意大的内存。
    TooLarge,
    /// body 不是合法 JSON，或指针没指到数组。**fail-open**：原样转发。
    NotJson,
    /// 解析成功但**没有条目被删** ⇒ 原样转发，连 framing 都不动。
    NothingRemoved,
    /// 裁剪本身成功，但**长度自检没过** ⇒ 退回原 body。
    ///
    /// 单独立一个原因（而不是并进 `NotJson`）：这是"我们自己的 bug"，
    /// 而 `NotJson` 是"对方的数据不是我们要的形状"。两者混在一起时，
    /// 排查只能靠猜。
    FramingRefused,
}

impl DeclineReason {
    /// 给日志/界面用的一句话（**中文，可读**；不要拿 `Debug` 糊用户）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NotConfigured => "未配置响应体裁剪",
            Self::NotJsonContentType => "响应不是 JSON 类型",
            Self::TooLarge => "响应体超过裁剪上限",
            Self::NotJson => "响应体不是合法 JSON 或指针不指向数组",
            Self::NothingRemoved => "没有条目需要删除",
            Self::FramingRefused => "裁剪后的长度自检没过（退回原响应体）",
        }
    }
}

/// 裁剪结果。
///
/// **故意不是 `Option<Vec<u8>>`**：`Unchanged` 必须带原因，否则"为什么没裁"
/// 只能靠猜，而这类功能最怕的就是"打开了但一直没生效、也没人发现"。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BodyRewrite {
    /// 真的要改：用这些字节（调用方**必须**同时重算 framing）。
    Changed(Vec<u8>),
    /// 不改：连同原因一起说明。
    Unchanged(DeclineReason),
}

/// 裁剪上限：设计文档 §8.5 写的「默认 ≤ 64 KiB」。
///
/// 注意这和 [`crate::proxy::MAX_BODY_BYTES`]（读全体的上限，12 MiB）不是一回事：
/// 前者是"愿意为裁剪额外付出的解析/内存成本"，后者是"愿意为一次响应缓冲多少"。
/// 两者都超出时选择**放行**，不选择失败。
pub const MAX_REWRITE_BYTES: usize = 64 * 1024;

/// 响应体裁剪的接缝。
///
/// 与判定用的 [`crate::decide::Decider`] 同一个套路：代理只依赖这个 trait，
/// 策略由上层（设置 → 桌面）装配，于是**代理不需要认识设置结构**。
pub trait BodyRewriter: Send + Sync {
    /// `content_type` 是响应头里的原样值（可能带 `; charset=utf-8`）。
    fn rewrite(
        &self,
        host: &str,
        path: &str,
        content_type: Option<&str>,
        body: &[u8],
    ) -> BodyRewrite;
}

/// 一个够用的实现：把 `pointer` 指到的数组里、`field == equals` 的条目删掉。
///
/// 例：`JsonStripRewriter::new("/data/items", "promoted", json!(true))`
/// 删掉 `data.items[]` 里 `promoted` 为 `true` 的条目。
///
/// **只认条目自己那一层的 `field`**（不做递归查找）：递归会让"删了哪些"
/// 变得难以预料，而这是要跟用户解释的行为。
pub struct JsonStripRewriter {
    pointer: String,
    field: String,
    equals: Value,
}

impl JsonStripRewriter {
    pub fn new(pointer: impl Into<String>, field: impl Into<String>, equals: Value) -> Self {
        Self { pointer: pointer.into(), field: field.into(), equals }
    }

    /// 裁剪目标（JSON 指针）。
    pub fn pointer(&self) -> &str {
        &self.pointer
    }
}

impl BodyRewriter for JsonStripRewriter {
    fn rewrite(
        &self,
        _host: &str,
        _path: &str,
        content_type: Option<&str>,
        body: &[u8],
    ) -> BodyRewrite {
        // 顺序有意为之：**先**用最便宜的检查挡掉不该处理的响应。
        if !content_type_is_json(content_type) {
            return BodyRewrite::Unchanged(DeclineReason::NotJsonContentType);
        }
        if body.len() > MAX_REWRITE_BYTES {
            return BodyRewrite::Unchanged(DeclineReason::TooLarge);
        }
        let (field, equals) = (self.field.clone(), self.equals.clone());
        match strip_json_array_entries(body, &self.pointer, move |item| {
            item.get(&field) == Some(&equals)
        }) {
            Ok(Some(new)) => BodyRewrite::Changed(new),
            Ok(None) => BodyRewrite::Unchanged(DeclineReason::NothingRemoved),
            Err(e) => {
                tracing::debug!(
                    error = %e,
                    pointer = %self.pointer,
                    "MITM：响应体裁剪放弃（fail-open，原样转发）"
                );
                BodyRewrite::Unchanged(DeclineReason::NotJson)
            }
        }
    }
}

/// JSON 的 MIME 判定：`application/json` 与 `*+json`（`; charset=...` 允许）。
fn content_type_is_json(content_type: Option<&str>) -> bool {
    let Some(ct) = content_type else {
        return false;
    };
    let mime = ct.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    mime == "application/json" || mime.ends_with("+json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// 一个"时间线接口"的样本：`data.items` 里有两条推广。
    fn timeline() -> Vec<u8> {
        serde_json::to_vec(&json!({
            "data": {
                "items": [
                    { "id": 1, "text": "ordinary" },
                    { "id": 2, "text": "promoted", "promoted": true },
                    { "id": 3, "text": "ordinary again" },
                    { "id": 4, "text": "promoted too", "promoted": true }
                ]
            }
        }))
        .unwrap()
    }

    #[test]
    fn stripping_promoted_entries_removes_only_those() {
        let out = strip_json_array_entries(&timeline(), "/data/items", |v| {
            v.get("promoted").and_then(Value::as_bool).unwrap_or(false)
        })
        .unwrap()
        .expect("应当有改动");
        let v: Value = serde_json::from_slice(&out).unwrap();
        let items = v["data"]["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert!(items.iter().all(|i| i.get("promoted").is_none()));
    }

    /// **没有删掉任何东西 ⇒ 返回 None**：调用方原样转发，连 framing 都不动。
    #[test]
    fn nothing_removed_means_no_rewrite_at_all() {
        let out = strip_json_array_entries(&timeline(), "/data/items", |_| false).unwrap();
        assert!(out.is_none());
    }

    /// 根就是数组的形态（`""` 指针）。
    #[test]
    fn the_root_pointer_works_for_a_top_level_array() {
        let body = serde_json::to_vec(&json!([1, 2, 3, 4])).unwrap();
        let out = strip_json_array_entries(&body, "", |v| v.as_u64() == Some(2)).unwrap().unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&out).unwrap(), json!([1, 3, 4]));
    }

    #[test]
    fn a_bad_pointer_or_non_json_body_is_an_error_not_a_guess() {
        assert!(matches!(
            strip_json_array_entries(&timeline(), "/nope/items", |_| true),
            Err(RewriteError::PathNotArray(_))
        ));
        // 指针指到对象（不是数组）也要报错，而不是"没删到"。
        assert!(matches!(
            strip_json_array_entries(&timeline(), "/data", |_| true),
            Err(RewriteError::PathNotArray(_))
        ));
        assert!(matches!(
            strip_json_array_entries(b"<html>not json</html>", "", |_| true),
            Err(RewriteError::NotJson(_))
        ));
    }

    /// **本模块存在的主要理由**：改 body 之后长度必须精确一致。
    #[test]
    fn changing_the_body_always_rewrites_the_length() {
        let mut headers = vec![
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Content-Length".to_string(), "999".to_string()),
        ];
        let new_body = br#"{"ok":true}"#;
        apply_body_change(&mut headers, new_body).unwrap();
        assert_eq!(
            crate::http1::get_header(&headers, "Content-Length"),
            Some(new_body.len().to_string().as_str())
        );
        assert!(length_matches(&headers, new_body).is_ok());
        // 旧长度的痕迹一点都不能留。
        assert!(!headers.iter().any(|(_, v)| v == "999"));
    }

    /// chunked 的响应被我们完整读进来之后，继续用 chunked 没有意义：
    /// 去掉 `Transfer-Encoding` 并写上精确长度（这是唯一不会不一致的做法）。
    #[test]
    fn a_chunked_response_is_converted_to_an_exact_content_length() {
        let mut headers = vec![
            ("Transfer-Encoding".to_string(), "chunked".to_string()),
            ("Content-Type".to_string(), "application/json".to_string()),
        ];
        let body = br#"[1,2]"#;
        apply_body_change(&mut headers, body).unwrap();
        assert!(crate::http1::get_header(&headers, "Transfer-Encoding").is_none());
        assert_eq!(crate::http1::get_header(&headers, "Content-Length"), Some("5"));
        assert!(length_matches(&headers, body).is_ok());
    }

    /// 既没有 Content-Length 也没有 chunked（HTTP/1.0 靠连接关闭分帧）：
    /// 我们**补上** Content-Length —— 否则放大 body 之后无法自检。
    #[test]
    fn a_response_without_any_framing_gets_a_content_length() {
        let mut headers = vec![("Content-Type".to_string(), "text/plain".to_string())];
        apply_body_change(&mut headers, b"hello").unwrap();
        assert_eq!(crate::http1::get_header(&headers, "Content-Length"), Some("5"));
    }

    /// 自检必须真的会拒绝——否则它只是一个装饰。
    #[test]
    fn the_self_check_refuses_a_mismatched_length() {
        let headers = vec![("Content-Length".to_string(), "3".to_string())];
        assert!(length_matches(&headers, b"hello").is_err());
        let none = vec![("Content-Type".to_string(), "x".to_string())];
        assert!(length_matches(&none, b"hello").is_err(), "没有长度也不能算通过");
    }

    /// 端到端形状：剪完 → 改头 → 自检，三步不能缺。
    #[test]
    fn strip_then_apply_then_check_is_a_consistent_pipeline() {
        let original = timeline();
        let mut headers = vec![
            ("Content-Length".to_string(), original.len().to_string()),
            ("Content-Type".to_string(), "application/json".to_string()),
        ];
        let stripped = strip_json_array_entries(&original, "/data/items", |v| {
            v.get("promoted").and_then(Value::as_bool).unwrap_or(false)
        })
        .unwrap()
        .unwrap();
        assert!(stripped.len() < original.len(), "剪完应该更短");
        apply_body_change(&mut headers, &stripped).unwrap();
        assert!(length_matches(&headers, &stripped).is_ok());
        // 旧的 Content-Length 若还在，这里就会红（这正是要防的那个 bug）。
        assert_eq!(
            crate::http1::get_header(&headers, "Content-Length"),
            Some(stripped.len().to_string().as_str())
        );
    }

    // ---- 接缝：谁裁、为什么没裁 -------------------------------------------

    /// **判别性**：字段命中就真删，并且剩下的字节仍是合法 JSON。
    #[test]
    fn the_rewriter_removes_a_matching_entry_and_keeps_valid_json() {
        let rw = JsonStripRewriter::new("/data/items", "promoted", Value::Bool(true));
        let out = rw.rewrite("x.test", "/api/timeline", Some("application/json"), &timeline());
        let BodyRewrite::Changed(new) = out else {
            panic!("命中一条 promoted 条目，应当真的改：{out:?}");
        };
        let v: Value = serde_json::from_slice(&new).expect("改完必须仍是合法 JSON");
        let items = v.pointer("/data/items").and_then(Value::as_array).unwrap();
        // 夹具是 4 条、其中 2 条 promoted ⇒ 剩下 2 条、且正好是 id 1 和 3。
        assert_eq!(items.len(), 2, "4 条里应当删掉恰好 2 条");
        let ids: Vec<&Value> = items.iter().map(|i| &i["id"]).collect();
        assert_eq!(ids, vec![&json!(1), &json!(3)], "留下的必须是没有 promoted 的两条");
        assert!(new.len() < timeline().len());
    }

    /// **负对照**：没有条目命中 ⇒ `NothingRemoved`，而且**字节一模一样**
    /// （连"改了个一模一样的东西"都不做）。
    #[test]
    fn no_matching_entry_declines_without_touching_bytes() {
        let rw = JsonStripRewriter::new("/data/items", "promoted", Value::Bool(true));
        let body = br#"{"data":{"items":[{"id":1},{"id":2}]}}"#;
        let out = rw.rewrite("x.test", "/api/timeline", Some("application/json"), body);
        assert_eq!(out, BodyRewrite::Unchanged(DeclineReason::NothingRemoved));
    }

    /// 不是 JSON 类型的响应**在解析前**就被挡掉（便宜的先做）。
    #[test]
    fn a_non_json_content_type_is_declined_before_parsing() {
        let rw = JsonStripRewriter::new("/data/items", "promoted", Value::Bool(true));
        let out = rw.rewrite("x.test", "/", Some("text/html; charset=utf-8"), &timeline());
        assert_eq!(out, BodyRewrite::Unchanged(DeclineReason::NotJsonContentType));
        let missing = rw.rewrite("x.test", "/", None, &timeline());
        assert_eq!(missing, BodyRewrite::Unchanged(DeclineReason::NotJsonContentType));
    }

    /// `application/*+json` 也要认（厂商 MIME 很常见），`; charset` 允许。
    #[test]
    fn vendor_json_mime_types_are_recognized() {
        let rw = JsonStripRewriter::new("/data/items", "promoted", Value::Bool(true));
        let out = rw.rewrite(
            "x.test",
            "/",
            Some("application/vnd.api+json; charset=utf-8"),
            &timeline(),
        );
        assert!(matches!(out, BodyRewrite::Changed(_)), "{out:?}");
    }

    /// 超过裁剪上限 ⇒ `TooLarge`，**不裁**（不为了一个响应缓冲任意大的内存）。
    #[test]
    fn an_oversized_body_is_declined_not_buffered() {
        let rw = JsonStripRewriter::new("/items", "promoted", Value::Bool(true));
        let mut items = Vec::new();
        for i in 0..(MAX_REWRITE_BYTES / 16) {
            items.push(json!({ "id": i, "promoted": true }));
        }
        let big = serde_json::to_vec(&json!({ "items": items })).unwrap();
        assert!(big.len() > MAX_REWRITE_BYTES, "夹具必须真的超过上限");
        let out = rw.rewrite("x.test", "/", Some("application/json"), &big);
        assert_eq!(out, BodyRewrite::Unchanged(DeclineReason::TooLarge));
    }

    /// body 说是 JSON 其实不是 ⇒ **fail-open**（不 panic、不把连接搞断）。
    #[test]
    fn a_broken_json_body_is_declined_instead_of_panicking() {
        let rw = JsonStripRewriter::new("/items", "promoted", Value::Bool(true));
        let out = rw.rewrite("x.test", "/", Some("application/json"), b"{not json");
        assert_eq!(out, BodyRewrite::Unchanged(DeclineReason::NotJson));
        // 指针不存在 / 指到的不是数组，同样只是"没改"。
        let missing = rw.rewrite("x.test", "/", Some("application/json"), b"{\"a\":1}");
        assert_eq!(missing, BodyRewrite::Unchanged(DeclineReason::NotJson));
    }

    /// 每种原因都有可读说法（不能只靠 `Debug`）。
    #[test]
    fn every_decline_reason_has_a_readable_explanation() {
        for r in [
            DeclineReason::NotConfigured,
            DeclineReason::NotJsonContentType,
            DeclineReason::TooLarge,
            DeclineReason::NotJson,
            DeclineReason::NothingRemoved,
            DeclineReason::FramingRefused,
        ] {
            assert!(!r.as_str().is_empty(), "{r:?} 必须能解释自己");
        }
    }
}
