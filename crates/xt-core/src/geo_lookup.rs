//! IP → 地理位置的**数据形状与合并逻辑**（不含网络请求）。
//!
//! # 为什么这里没有 HTTP 代码
//!
//! 位置查询本来放在这个 crate 里、用自写 socket 发请求。但换成 https 需要 TLS，
//! 而项目已有既定做法：**用系统的 `/usr/bin/curl`**（零依赖、走系统信任链、
//! 支持 `--interface` 绑网卡），见 `commands/nodes.rs` 的订阅拉取。
//!
//! 所以这里只留**纯逻辑**：响应形状、解析、双源合并。网络请求在
//! `apps/desktop/src/commands/globe.rs`（那边有异步运行时与 curl 调用惯例）。
//! 好处是这些判定不必发网络请求就能测。
//!
//! # 关于「位置准不准」
//!
//! IP 地理定位是**尽力而为**：同一个 IP 不同服务可能给出不同城市，
//! 运营商大内网（CGNAT）与省级骨干出口会让注册地偏离实际所在城市。
//! 实测过一个反例：同一台机器的公网 IP 在 `ip-api.com` 上被判到广州、
//! 而 `ipwho.is` 与 `ipinfo.io` 都指向大理。所以这里**同时问两个源**，
//! 坐标不一致时把 `consistent` 标为 false，由界面如实说明。

use serde::{Deserialize, Serialize};

/// 查到的位置。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeoLocation {
    pub ip: String,
    pub country: String,
    pub city: String,
    pub lat: f64,
    pub lon: f64,
    #[serde(default)]
    pub isp: String,
    /// 坐标来源，界面据此如实标注。
    pub source: String,
    /// 多个数据源对**同一个 IP** 的判定是否一致。
    #[serde(default)]
    pub consistent: bool,
    /// 各数据源的判定摘要，便于界面展示分歧。
    #[serde(default)]
    pub sources: Vec<String>,
}

/// 两个源的地理位置相差超过这个度数就认为「不一致」。
///
/// 0.5° 约 55 公里：城市级判定的正常差异（不同服务的城市中心点不同）应当
/// 小于它，而「判到另一个城市」通常远大于它。
pub const CONSISTENT_TOLERANCE_DEG: f64 = 0.5;

/// 合并两个数据源的判定。
///
/// * 坐标以 `primary`（ipwho.is）为准 —— 实测它对本机公网 IP 的判定与
///   `ipinfo.io` 一致地指向用户实际所在的大理，而 `ip-api.com` 免费版
///   对省级骨干 IP 的区级判定偏差更大；
/// * **中文地名用 `secondary`（ip-api.com）** —— 它支持 `lang=zh-CN`，
///   返回「大理」而不是 `Dali Baizu Zizhizhou`；
/// * 两者坐标相差超过 [`CONSISTENT_TOLERANCE_DEG`] 时 `consistent = false`。
pub fn merge_sources(
    _ip: &str,
    primary: Option<GeoLocation>,
    secondary: Option<GeoLocation>,
) -> Option<GeoLocation> {
    match (primary, secondary) {
        (None, None) => None,
        (Some(mut a), None) => {
            a.consistent = true;
            a.sources = vec![format!("{}: {}", a.source, round2(a.lat))];
            Some(a)
        }
        (None, Some(mut b)) => {
            b.consistent = true;
            b.sources = vec![format!("{}: {}", b.source, round2(b.lat))];
            Some(b)
        }
        (Some(mut a), Some(b)) => {
            let far = (a.lat - b.lat).abs() > CONSISTENT_TOLERANCE_DEG
                || (a.lon - b.lon).abs() > CONSISTENT_TOLERANCE_DEG;
            // 中文地名优先（判据是「含非 ASCII 字符」，与具体服务无关）
            if !b.city.is_ascii() {
                a.city = b.city.clone();
            }
            if !b.country.is_empty() {
                a.country = b.country.clone();
            }
            if !b.isp.is_empty() {
                a.isp = b.isp.clone();
            }
            a.consistent = !far;
            a.sources = vec![
                format!("{}: {}", a.source, round2(a.lat)),
                format!("{}: {}", b.source, round2(b.lat)),
            ];
            Some(a)
        }
    }
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// 解析 `ipwho.is` 的响应。
pub fn parse_who(body: &str, fallback_ip: &str) -> Option<GeoLocation> {
    #[derive(Deserialize)]
    struct Who {
        #[serde(default)]
        success: bool,
        #[serde(default)]
        ip: String,
        #[serde(default)]
        city: String,
        #[serde(default)]
        country: String,
        #[serde(default)]
        latitude: f64,
        #[serde(default)]
        longitude: f64,
        #[serde(default)]
        connection: Option<WhoConn>,
    }
    #[derive(Deserialize)]
    struct WhoConn {
        #[serde(default)]
        isp: String,
        #[serde(default)]
        org: String,
    }
    let p: Who = serde_json::from_str(body).ok()?;
    if !p.success {
        return None;
    }
    let isp = p.connection.map_or_else(String::new, |c| {
        if c.isp.is_empty() {
            c.org
        } else {
            c.isp
        }
    });
    Some(GeoLocation {
        ip: if p.ip.is_empty() {
            fallback_ip.to_string()
        } else {
            p.ip
        },
        country: p.country,
        city: p.city,
        lat: p.latitude,
        lon: p.longitude,
        isp,
        source: "ipwho.is".into(),
        consistent: true,
        sources: Vec::new(),
    })
}

/// 解析 `ip-api.com` 的响应（`lang=zh-CN`，所以地名是中文）。
pub fn parse_api(body: &str, fallback_ip: &str) -> Option<GeoLocation> {
    #[derive(Deserialize)]
    struct Api {
        status: String,
        #[serde(default)]
        query: String,
        #[serde(default)]
        country: String,
        #[serde(default)]
        city: String,
        #[serde(default)]
        lat: f64,
        #[serde(default)]
        lon: f64,
        #[serde(default)]
        isp: String,
    }
    let p: Api = serde_json::from_str(body).ok()?;
    if p.status != "success" {
        return None;
    }
    Some(GeoLocation {
        ip: if p.query.is_empty() {
            fallback_ip.to_string()
        } else {
            p.query
        },
        country: p.country,
        city: p.city,
        lat: p.lat,
        lon: p.lon,
        isp: p.isp,
        source: "ip-api.com".into(),
        consistent: true,
        sources: Vec::new(),
    })
}

/// 位置缓存：同一个 IP 在一次查询内只查一次。
///
/// 两层理由：外部服务有限流，而且每多查一次就多一次「把 IP 发给第三方」。
///
/// **刻意不跨请求保留**：用户的公网 IP 会变（实测过
/// `45.207.197.185` → `39.130.21.95` → `116.53.173.241`），
/// 长期缓存会让界面一直显示上一个 IP 的位置。
#[derive(Debug, Default)]
pub struct GeoCache {
    map: std::collections::HashMap<String, GeoLocation>,
}

impl GeoCache {
    pub fn get(&self, key: &str) -> Option<GeoLocation> {
        self.map.get(key).cloned()
    }

    pub fn put(&mut self, key: &str, loc: GeoLocation) {
        self.map.insert(key.to_string(), loc);
    }
}

/// 本机位置的缓存键：它不是按被查 IP 缓存的，用一个固定键。
pub const SELF_KEY: &str = "__self__";

#[cfg(test)]
mod tests {
    use super::*;

    fn loc(source: &str, city: &str, lat: f64, lon: f64) -> GeoLocation {
        GeoLocation {
            ip: "1.2.3.4".into(),
            country: "中国".into(),
            city: city.into(),
            lat,
            lon,
            isp: String::new(),
            source: source.into(),
            consistent: true,
            sources: Vec::new(),
        }
    }

    #[test]
    fn parses_a_successful_who_response() {
        let body = r#"{"success":true,"ip":"116.53.173.241","country":"China",
                       "city":"Dali Baizu Zizhizhou","latitude":25.6074778,"longitude":100.2651597,
                       "connection":{"isp":"CHINANET","org":"YunNan"}}"#;
        let l = parse_who(body, "").expect("应当解析成功");
        assert_eq!(l.ip, "116.53.173.241");
        assert!((l.lat - 25.6074778).abs() < 1e-6);
        assert_eq!(l.isp, "CHINANET");
    }

    #[test]
    fn parses_a_successful_api_response_with_chinese_city() {
        let body = r#"{"status":"success","query":"116.53.173.241","country":"中国",
                       "city":"大理","lat":25.6886,"lon":100.159,"isp":"电信"}"#;
        let l = parse_api(body, "").expect("应当解析成功");
        assert_eq!(l.city, "大理");
        assert_eq!(l.source, "ip-api.com");
    }

    #[test]
    fn failure_responses_are_rejected() {
        assert!(parse_api(r#"{"status":"fail","message":"private range"}"#, "").is_none());
        assert!(parse_who(r#"{"success":false,"message":"invalid IP"}"#, "").is_none());
        assert!(parse_api("not json", "").is_none());
    }

    /// 两个源一致：坐标取主源，**中文地名取次源** —— 这正是想要的组合
    /// （ipwho.is 更准但只有英文，ip-api 支持中文）。
    #[test]
    fn merge_prefers_primary_coords_and_chinese_city() {
        let who = loc("ipwho.is", "Dali Baizu Zizhizhou", 25.607, 100.265);
        let api = loc("ip-api.com", "大理", 25.689, 100.159);
        let m = merge_sources("1.2.3.4", Some(who), Some(api)).unwrap();
        assert_eq!(m.city, "大理", "中文地名应当来自 ip-api");
        assert!((m.lat - 25.607).abs() < 1e-6, "坐标应当来自 ipwho.is");
        assert!(m.consistent);
        assert_eq!(m.sources.len(), 2);
    }

    /// 两个源差得远：标注不一致，界面才能说明「按 IP 归属估算」。
    #[test]
    fn merge_flags_disagreement() {
        let who = loc("ipwho.is", "Dali", 25.6, 100.2);
        let api = loc("ip-api.com", "广州市", 23.1, 113.2);
        let m = merge_sources("1.2.3.4", Some(who), Some(api)).unwrap();
        assert!(!m.consistent, "相差十几度必须标为不一致");
        assert_eq!(m.sources.len(), 2, "两个源的判定都要能展示出来");
    }

    /// 只有一个源时 sources 只有一条，界面据此说明来源单一。
    #[test]
    fn single_source_is_marked_by_source_list() {
        let only = loc("ipwho.is", "Dali", 25.6, 100.2);
        let m = merge_sources("1.2.3.4", Some(only), None).unwrap();
        assert_eq!(m.sources.len(), 1);
        assert!(m.sources[0].contains("ipwho.is"));
    }

    #[test]
    fn both_missing_yields_none() {
        assert!(merge_sources("1.2.3.4", None, None).is_none());
    }
}
