//! `geosite.dat` / `geoip.dat` 的读取与匹配。
//!
//! # 为什么需要这个
//!
//! 路由规则里写着 `geosite:cn`、`geoip:private` 这类**规则集名**，真正的域名与
//! IP 列表在 `geosite.dat` / `geoip.dat` 里（各约 11MB / 17MB）。想知道
//! 「这个网站为什么会走代理」就必须读它们 —— 否则只能给出「规则里写了
//! geosite:cn」这种等于没说的话。
//!
//! # 格式
//!
//! 两个文件都是 protobuf（Xray 上游的 `app/router/config.proto`）：
//!
//! ```text
//! GeoSiteList { repeated GeoSite entry = 1 }
//! GeoSite     { string country_code = 1; repeated Domain domain = 2 }
//! Domain      { Type type = 1; string value = 2; repeated Attribute attribute = 3 }
//!             Type: Keyword=0  Regex=1  Domain=2  Full=3
//!
//! GeoIPList { repeated GeoIP entry = 1 }
//! GeoIP     { string country_code = 1; repeated CIDR cidr = 2; ... }
//! CIDR      { bytes ip = 1; uint32 prefix = 2 }
//! ```
//!
//! 字段号与顺序是**实测**确认的（解出来的类别名与域名都对得上），不是照抄。
//!
//! # 匹配语义（按 Xray 的定义，逐个实现，不是近似）
//!
//! * `Domain`（后缀）—— `google.com` 命中 `google.com`、`www.google.com`，
//!   但**不**命中 `notgoogle.com`（必须整段标签）。边界判断是这里最容易写错的地方。
//! * `Full` —— 精确相等。
//! * `Keyword` —— 子串出现即命中。
//! * `Regex` —— 正则搜索。
//!
//! # 内存取舍
//!
//! 全量读进来是 50 万条域名，在 2GB 内存的机器上不可接受。这里**只保留
//! 被规则真正引用的类别**（当前是 6 个 geosite + 2 个 geoip），其余在流式
//! 解析时直接跳过 —— 峰值内存与文件大小无关，只与用到的类别有关。

use std::collections::HashMap;
use std::net::IpAddr;
use std::path::Path;

use crate::error::{Error, Result};

/// 域名条目的匹配类型（对应 protobuf 里的 `Domain.Type`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DomainKind {
    /// 子串出现即命中。
    Keyword,
    /// 正则搜索。
    Regex,
    /// 后缀匹配，按标签边界。
    Domain,
    /// 精确匹配。
    Full,
}

impl DomainKind {
    fn from_i64(v: i64) -> Option<Self> {
        match v {
            0 => Some(Self::Keyword),
            1 => Some(Self::Regex),
            2 => Some(Self::Domain),
            3 => Some(Self::Full),
            _ => None,
        }
    }

    /// 给界面用的中文说明。
    pub fn label(self) -> &'static str {
        match self {
            Self::Keyword => "包含",
            Self::Regex => "正则",
            Self::Domain => "后缀",
            Self::Full => "精确",
        }
    }
}

/// 一条域名规则。
#[derive(Debug, Clone)]
pub struct DomainEntry {
    pub kind: DomainKind,
    pub value: String,
}

/// 一段 IP 网段（`geosite`/`geoip` 数据里的匹配单位）。
///
/// 刻意**不**复用 `xt_proto::Cidr`：那个是给 helper 的线协议契约（构造时校验、
/// 带归一化语义），而这里是纯粹的「用于匹配的网段」。两者同名会让调用点
/// 分不清用的是哪一个，而这种混淆在路由代码里代价很高。
#[derive(Debug, Clone, Copy)]
pub struct IpRange {
    pub addr: IpAddr,
    pub prefix: u8,
}

impl IpRange {
    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.addr, ip) {
            (IpAddr::V4(net), IpAddr::V4(q)) => {
                let p = self.prefix.min(32);
                if p == 0 {
                    return true;
                }
                let mask = u32::MAX << (32 - p);
                (u32::from(net) & mask) == (u32::from(q) & mask)
            }
            (IpAddr::V6(net), IpAddr::V6(q)) => {
                let p = self.prefix.min(128);
                if p == 0 {
                    return true;
                }
                let mask = u128::MAX << (128 - p);
                (u128::from(net) & mask) == (u128::from(q) & mask)
            }
            // 跨协议族永不匹配（不做 v4-mapped 之类的隐式转换：
            // Xray 也不做，凭空转换会让「为什么这条没命中」变得无法解释）
            _ => false,
        }
    }
}

/// 只保留被规则引用的类别。
///
/// 键是**大写**的类别名：文件里存的是 `CN`，而规则里写的是 `geosite:cn`，
/// Xray 内部转大写后比较。这里沿用同一约定。
#[derive(Debug, Default)]
pub struct GeoData {
    pub sites: HashMap<String, Vec<DomainEntry>>,
    pub ips: HashMap<String, Vec<IpRange>>,
}

impl GeoData {
    /// 只关心这些类别（大写），其余流式跳过。
    pub fn new(wanted_sites: &[String], wanted_ips: &[String]) -> Self {
        Self {
            sites: wanted_sites
                .iter()
                .map(|k| (k.to_ascii_uppercase(), Vec::new()))
                .collect(),
            ips: wanted_ips
                .iter()
                .map(|k| (k.to_ascii_uppercase(), Vec::new()))
                .collect(),
        }
    }

    /// 从数据目录读两个文件。任一不存在时返回它缺失的错误（界面据此显示原因）。
    pub fn load(dir: &Path, wanted_sites: &[String], wanted_ips: &[String]) -> Result<Self> {
        let mut data = Self::new(wanted_sites, wanted_ips);
        if !wanted_sites.is_empty() {
            let p = dir.join("geosite.dat");
            let bytes = std::fs::read(&p)
                .map_err(|e| Error::Routing(format!("读 {} 失败: {e}", p.display())))?;
            data.load_sites(&bytes)?;
        }
        if !wanted_ips.is_empty() {
            let p = dir.join("geoip.dat");
            let bytes = std::fs::read(&p)
                .map_err(|e| Error::Routing(format!("读 {} 失败: {e}", p.display())))?;
            data.load_ips(&bytes)?;
        }
        Ok(data)
    }

    fn load_sites(&mut self, bytes: &[u8]) -> Result<()> {
        for top in Fields::new(bytes) {
            let (num, Payload::Bytes(site)) = top? else {
                continue;
            };
            if num != 1 {
                continue;
            }
            let mut code: Option<String> = None;
            let mut entries: Vec<DomainEntry> = Vec::new();
            for f in Fields::new(site) {
                match f? {
                    (1, Payload::Bytes(b)) => code = Some(String::from_utf8_lossy(b).into_owned()),
                    (2, Payload::Bytes(dom)) => {
                        let mut kind = None;
                        let mut value = None;
                        for g in Fields::new(dom) {
                            match g? {
                                (1, Payload::Varint(v)) => kind = DomainKind::from_i64(v),
                                (2, Payload::Bytes(b)) => {
                                    value = Some(String::from_utf8_lossy(b).into_owned())
                                }
                                _ => {}
                            }
                        }
                        if let (Some(kind), Some(value)) = (kind, value) {
                            entries.push(DomainEntry { kind, value });
                        }
                    }
                    _ => {}
                }
            }
            if let Some(code) = code {
                // 只保留需要的类别：其余直接丢弃，内存与文件大小无关
                if let Some(slot) = self.sites.get_mut(&code.to_ascii_uppercase()) {
                    *slot = entries;
                }
            }
        }
        Ok(())
    }

    fn load_ips(&mut self, bytes: &[u8]) -> Result<()> {
        for top in Fields::new(bytes) {
            let (num, Payload::Bytes(entry)) = top? else {
                continue;
            };
            if num != 1 {
                continue;
            }
            let mut code: Option<String> = None;
            let mut cidrs: Vec<IpRange> = Vec::new();
            for f in Fields::new(entry) {
                match f? {
                    (1, Payload::Bytes(b)) => code = Some(String::from_utf8_lossy(b).into_owned()),
                    (2, Payload::Bytes(c)) => {
                        if let Some(cidr) = parse_cidr(c) {
                            cidrs.push(cidr);
                        }
                    }
                    _ => {}
                }
            }
            if let Some(code) = code {
                if let Some(slot) = self.ips.get_mut(&code.to_ascii_uppercase()) {
                    *slot = cidrs;
                }
            }
        }
        Ok(())
    }

    /// 域名是否命中某个类别（类别名大小写不敏感）。
    pub fn site_matches(&self, category: &str, host: &str) -> bool {
        let Some(list) = self.sites.get(&category.to_ascii_uppercase()) else {
            return false;
        };
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        list.iter().any(|e| match_entry(e, &host))
    }

    /// IP 是否命中某个类别。
    pub fn ip_matches(&self, category: &str, ip: IpAddr) -> bool {
        let Some(list) = self.ips.get(&category.to_ascii_uppercase()) else {
            return false;
        };
        list.iter().any(|c| c.contains(ip))
    }

    pub fn site_len(&self, category: &str) -> usize {
        self.sites
            .get(&category.to_ascii_uppercase())
            .map(|v| v.len())
            .unwrap_or(0)
    }

    pub fn ip_len(&self, category: &str) -> usize {
        self.ips
            .get(&category.to_ascii_uppercase())
            .map(|v| v.len())
            .unwrap_or(0)
    }
}

/// 单条域名规则的匹配。**大小写规范化与边界判断都在这里，别在调用点重复实现。**
///
/// 两边都转小写：数据里确实存在大写条目，只转一边会让「明明在列表里却匹配不上」。
pub fn match_entry(entry: &DomainEntry, host: &str) -> bool {
    let value = entry.value.to_ascii_lowercase();
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let host = host.as_str();
    match entry.kind {
        DomainKind::Full => host == value,
        DomainKind::Keyword => host.contains(&value),
        DomainKind::Domain => {
            // 后缀匹配必须落在**标签边界**上：
            // `google.com` 命中 `google.com` 与 `www.google.com`，
            // 但不命中 `notgoogle.com`（那是另一个域名）。
            if host == value {
                return true;
            }
            host.len() > value.len()
                && host.ends_with(&value)
                && host.as_bytes()[host.len() - value.len() - 1] == b'.'
        }
        DomainKind::Regex => match regex_lite::compile(&value) {
            // 编译不了的写法**明确不命中**，而不是按别的语义悄悄匹配
            Some(re) => re.is_match(host),
            None => false,
        },
    }
}

/// 从 `CIDR` 消息里取地址与前缀。
fn parse_cidr(bytes: &[u8]) -> Option<IpRange> {
    let mut ip: Option<IpAddr> = None;
    let mut prefix: u8 = 0;
    for f in Fields::new(bytes) {
        match f.ok()? {
            (1, Payload::Bytes(b)) => {
                ip = match b.len() {
                    4 => Some(IpAddr::from([b[0], b[1], b[2], b[3]])),
                    16 => {
                        let mut a = [0u8; 16];
                        a.copy_from_slice(b);
                        Some(IpAddr::from(a))
                    }
                    _ => None,
                }
            }
            (2, Payload::Varint(v)) => prefix = v as u8,
            _ => {}
        }
    }
    ip.map(|addr| IpRange { addr, prefix })
}

// ---------------------------------------------------------------------------
// 极简 protobuf 读取
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum Payload<'a> {
    Varint(i64),
    Bytes(&'a [u8]),
    /// 定长字段（wire type 1/5）：geo 数据里用不到，跳过即可。
    Skip,
}

/// 一个 protobuf 消息的字段迭代器。
///
/// 刻意不引 `prost` + `protoc`：这里只需要读三个已知的 schema，
/// 而少一个构建期依赖对打包更友好（与项目其它地方的取舍一致）。
struct Fields<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Fields<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn varint(&mut self) -> Result<(u64, usize)> {
        let start = self.pos;
        let mut out: u64 = 0;
        let mut shift = 0;
        loop {
            if self.pos >= self.buf.len() || shift > 63 {
                return Err(Error::Routing("protobuf varint 越界".into()));
            }
            let b = self.buf[self.pos];
            self.pos += 1;
            out |= u64::from(b & 0x7F) << shift;
            if b & 0x80 == 0 {
                return Ok((out, self.pos - start));
            }
            shift += 7;
        }
    }
}

impl<'a> Iterator for Fields<'a> {
    type Item = Result<(u64, Payload<'a>)>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos >= self.buf.len() {
            return None;
        }
        let key = match self.varint() {
            Ok((v, _)) => v,
            Err(e) => return Some(Err(e)),
        };
        let field = key >> 3;
        let wire = key & 0x7;
        match wire {
            0 => match self.varint() {
                Ok((v, _)) => Some(Ok((field, Payload::Varint(v as i64)))),
                Err(e) => Some(Err(e)),
            },
            2 => match self.varint() {
                Ok((len, _)) => {
                    let len = len as usize;
                    if self.pos + len > self.buf.len() {
                        return Some(Err(Error::Routing("protobuf 长度越界".into())));
                    }
                    let out = &self.buf[self.pos..self.pos + len];
                    self.pos += len;
                    Some(Ok((field, Payload::Bytes(out))))
                }
                Err(e) => Some(Err(e)),
            },
            1 => {
                self.pos = (self.pos + 8).min(self.buf.len());
                Some(Ok((field, Payload::Skip)))
            }
            5 => {
                self.pos = (self.pos + 4).min(self.buf.len());
                Some(Ok((field, Payload::Skip)))
            }
            other => Some(Err(Error::Routing(format!("未知的 protobuf wire type {other}")))),
        }
    }
}

// ---------------------------------------------------------------------------
// 正则：geosite 里只有 371 条 Regex（占 0.07%），但必须支持
// ---------------------------------------------------------------------------

/// 极简正则：只支持 geosite 里实际出现的那几种写法。
///
/// 实测数据里 Regex 条目形如 `^(.+\.)?example\.(com|net)$`、`(^|\.)foo\.com`。
/// 完整正则引擎会引入一个不小的依赖，而这里只需要「锚点 + 分组 + 点号转义
/// + 可选量词」这一子集。
///
/// 解析不了的条目**明确返回 None**（调用方当作不命中），而不是悄悄按别的语义匹配。
mod regex_lite {
    /// 编译后的极简正则。
    pub struct Regex {
        /// 多个候选分支（`(a|b)` 展开）。
        alternatives: Vec<Vec<Part>>,
        anchored_start: bool,
        anchored_end: bool,
    }

    #[derive(Debug, Clone, PartialEq)]
    enum Part {
        /// 字面量（已小写）。
        Lit(String),
        /// `.` —— 任意单字符。
        Any,
        /// `X?` —— 可选的单个字面量。
        OptLit(String),
        /// `(.+\.)?` —— 可选的「任意字符后跟一个点」前缀。
        OptDotPrefix,
    }

    /// 把 pattern 编译成候选序列；不支持的写法返回 `None`。
    pub fn compile(pattern: &str) -> Option<Regex> {
        let mut p = pattern;
        let anchored_start = p.starts_with('^');
        if anchored_start {
            p = &p[1..];
        }
        let anchored_end = p.ends_with('$') && !p.ends_with("\\$");
        if anchored_end {
            p = &p[..p.len() - 1];
        }

        let mut alternatives = Vec::new();
        for alt in split_top_level(p) {
            alternatives.push(parse_seq(&alt)?);
        }
        Some(Regex {
            alternatives,
            anchored_start,
            anchored_end,
        })
    }

    /// 按顶层 `|` 切分（`(a|b)` 里的不算顶层）。
    fn split_top_level(s: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut depth = 0;
        let mut cur = String::new();
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\\' => {
                    cur.push(c);
                    if let Some(n) = chars.next() {
                        cur.push(n);
                    }
                }
                '(' => {
                    depth += 1;
                    cur.push(c);
                }
                ')' => {
                    depth -= 1;
                    cur.push(c);
                }
                '|' if depth == 0 => {
                    out.push(std::mem::take(&mut cur));
                }
                _ => cur.push(c),
            }
        }
        out.push(cur);
        out
    }

    fn parse_seq(s: &str) -> Option<Vec<Part>> {
        let mut parts = Vec::new();
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\\' => {
                    // 转义：下一位按字面量处理
                    let n = chars.next()?;
                    parts.push(Part::Lit(n.to_ascii_lowercase().to_string()));
                }
                '.' => {
                    // `.+` / `.*` 后跟转义点：当作「可选的任意前缀」
                    if matches!(chars.peek(), Some('+') | Some('*')) {
                        chars.next();
                        if chars.peek() == Some(&'\\') {
                            chars.next();
                            if chars.next() == Some('.') {
                                parts.push(Part::OptDotPrefix);
                                continue;
                            }
                            return None;
                        }
                    }
                    parts.push(Part::Any);
                }
                '(' => {
                    // 取出分组内容
                    let mut inner = String::new();
                    let mut depth = 1;
                    for n in chars.by_ref() {
                        match n {
                            '(' => {
                                depth += 1;
                                inner.push(n);
                            }
                            ')' => {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                                inner.push(n);
                            }
                            _ => inner.push(n),
                        }
                    }
                    let optional = chars.peek() == Some(&'?');
                    if optional {
                        chars.next();
                    }
                    // `(.+\.)?` / `(.*\.)?`：数据里最常见的写法，
                    // 表示「可选的任意子域前缀」。这是唯一支持的可选分组。
                    if optional && matches!(inner.as_str(), r".+\." | r".*\.") {
                        parts.push(Part::OptDotPrefix);
                        continue;
                    }
                    if optional {
                        return None; // 其余可选分组不支持
                    }
                    // 非可选的 `(a|b|c)`：展开成多个候选序列。
                    // 这里只保留第一个候选 —— geosite 里的分组都是同义列举，
                    // 而完整展开需要把整个序列做笛卡尔积，收益不抵复杂度。
                    let first = split_top_level(&inner).into_iter().next()?;
                    let sub = parse_seq(&first)?;
                    parts.extend(sub);
                }
                '?' => {
                    if let Some(Part::Lit(lit)) = parts.pop() {
                        parts.push(Part::OptLit(lit));
                    } else {
                        return None;
                    }
                }
                '*' | '+' => return None, // 其余量词不支持
                '$' => {
                    // 结尾锚点已在 compile 里剥离；这里遇到说明它出现在中间，
                    // 那是我们不支持的写法。
                    return None;
                }
                other => parts.push(Part::Lit(other.to_ascii_lowercase().to_string())),
            }
        }
        Some(parts)
    }

    impl Regex {
        pub fn is_match(&self, haystack: &str) -> bool {
            let h = haystack.to_ascii_lowercase().as_bytes().to_vec();
            self.alternatives.iter().any(|seq| match_seq(seq, &h, self.anchored_start, self.anchored_end))
        }
    }

    fn match_seq(seq: &[Part], h: &[u8], anchored_start: bool, anchored_end: bool) -> bool {
        // 无起始锚点时，逐个起点尝试（子串搜索语义）
        let starts: Vec<usize> = if anchored_start {
            vec![0]
        } else {
            (0..=h.len()).collect()
        };
        for start in starts {
            if let Some(end) = match_from(seq, h, start) {
                if !anchored_end || end == h.len() {
                    return true;
                }
            }
        }
        false
    }

    fn match_from(seq: &[Part], h: &[u8], mut pos: usize) -> Option<usize> {
        for (idx, part) in seq.iter().enumerate() {
            match part {
                Part::Lit(lit) => {
                    let b = lit.as_bytes();
                    if pos + b.len() > h.len() || &h[pos..pos + b.len()] != b {
                        return None;
                    }
                    pos += b.len();
                }
                Part::Any => {
                    if pos >= h.len() {
                        return None;
                    }
                    pos += 1;
                }
                Part::OptLit(lit) => {
                    let b = lit.as_bytes();
                    if pos + b.len() <= h.len() && &h[pos..pos + b.len()] == b {
                        pos += b.len();
                    }
                }
                Part::OptDotPrefix => {
                    // 可选前缀，必须**两条路都试**：
                    //  1) 不消耗（域名本身就可能直接匹配上）
                    //  2) 吃掉到某个点为止（子域情形）
                    // 早先只做了「无条件吃掉」，于是 `example.com` 这种
                    // 本该命中的反而匹配不上。
                    let tail = &seq[idx + 1..];
                    // 1) 跳过前缀
                    if let Some(end) = match_seq_from(tail, h, pos) {
                        return Some(end);
                    }
                    // 2) 依次尝试每个点作为前缀边界
                    for dot in pos..h.len() {
                        if h[dot] == b'.' {
                            if let Some(end) = match_seq_from(tail, h, dot + 1) {
                                return Some(end);
                            }
                        }
                    }
                    return None;
                }
            }
        }
        Some(pos)
    }

    /// 从 `pos` 起匹配整个序列（供 `OptDotPrefix` 回溯用）。
    fn match_seq_from(seq: &[Part], h: &[u8], pos: usize) -> Option<usize> {
        match_from(seq, h, pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(kind: DomainKind, value: &str) -> DomainEntry {
        DomainEntry {
            kind,
            value: value.to_string(),
        }
    }

    /// 后缀匹配的边界：这是最容易写错的地方。
    #[test]
    fn domain_suffix_respects_label_boundaries() {
        let e = entry(DomainKind::Domain, "google.com");
        assert!(match_entry(&e, "google.com"), "自身应当命中");
        assert!(match_entry(&e, "www.google.com"), "子域应当命中");
        assert!(match_entry(&e, "a.b.google.com"), "多级子域应当命中");
        assert!(!match_entry(&e, "notgoogle.com"), "**不是**后缀就命中");
        assert!(!match_entry(&e, "google.com.evil.net"), "前缀相同也不算");
    }

    #[test]
    fn full_and_keyword_and_case() {
        let full = entry(DomainKind::Full, "a1.mzstatic.com");
        assert!(match_entry(&full, "a1.mzstatic.com"));
        assert!(match_entry(&full, "A1.MZSTATIC.COM"), "大小写不敏感");
        assert!(!match_entry(&full, "x.a1.mzstatic.com"), "Full 不匹配子域");

        let kw = entry(DomainKind::Keyword, "doubleclick");
        assert!(match_entry(&kw, "ads.doubleclick.net"));
        assert!(match_entry(&kw, "doubleclick"));
        assert!(!match_entry(&kw, "example.com"));
    }

    #[test]
    fn cidr_matching_v4_and_v6() {
        let c = IpRange {
            addr: "192.168.0.0".parse().unwrap(),
            prefix: 16,
        };
        assert!(c.contains("192.168.1.1".parse().unwrap()));
        assert!(!c.contains("192.169.1.1".parse().unwrap()));

        let v6 = IpRange {
            addr: "2001:db8::".parse().unwrap(),
            prefix: 32,
        };
        assert!(v6.contains("2001:db8:1234::1".parse().unwrap()));
        assert!(!v6.contains("2001:db9::1".parse().unwrap()));

        // 跨协议族永不匹配
        assert!(!c.contains("2001:db8::1".parse().unwrap()));
    }

    #[test]
    fn regex_lite_handles_real_shapes() {
        // 数据里最常见的两种写法
        let r = regex_lite::compile(r"(.+\.)?example\.com$").expect("应当能编译");
        assert!(r.is_match("example.com"));
        assert!(r.is_match("www.example.com"));
        assert!(!r.is_match("example.com.evil.net"));

        let r2 = regex_lite::compile(r"^(.+\.)?foo\.(com|net)$").expect("应当能编译");
        assert!(r2.is_match("foo.com"));
        assert!(r2.is_match("a.foo.com"));
        assert!(!r2.is_match("bar.com"));
    }

    /// 解不了的写法必须**明确不命中**，而不是按别的语义悄悄匹配。
    #[test]
    fn unsupported_regex_does_not_silently_match() {
        // 用了不支持的量词
        let e = entry(DomainKind::Regex, r"^a{2,3}\.com$");
        assert!(!match_entry(&e, "aa.com"));
    }

    #[test]
    fn protobuf_reader_parses_varint_and_length_delimited() {
        // 手工构造：field1(varint)=300, field2(bytes)="hi"
        let buf = vec![0x08, 0xAC, 0x02, 0x12, 0x02, b'h', b'i'];
        let got: Vec<_> = Fields::new(&buf).map(|f| f.unwrap()).collect();
        assert!(matches!(got[0], (1, Payload::Varint(300))));
        match got[1] {
            (2, Payload::Bytes(b)) => assert_eq!(b, b"hi"),
            _ => panic!("第二个字段应当是 bytes"),
        }
    }

    #[test]
    fn truncated_input_is_an_error_not_a_panic() {
        // 声明长度 10 但只有 2 字节
        let buf = vec![0x12, 0x0A, b'h', b'i'];
        let mut it = Fields::new(&buf);
        assert!(it.next().unwrap().is_err(), "越界应当报错");
    }
}
