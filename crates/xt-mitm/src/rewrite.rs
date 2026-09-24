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
}
