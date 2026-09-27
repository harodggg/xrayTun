//! 观察模式测试的公共替身：**可编排源站**、SOCKS 转发器、TLS 客户端。
//!
//! 与 `e2e.rs` 里的替身是同一个套路，但这里多了一件事：源站把**收到的请求 body
//! 原始字节**记下来。判据 1（「POST body 逐字节不变」）只能靠它 ——
//! 没有它，我们只能证明"请求没报错"，证明不了"字节没变"。
//!
//! 这里**只用 xt-mitm 现有的公开 API**（`serve`/`serve_with`/`ProxyConfig`），
//! 这样判据 1/2 的测试可以在实现观察模式**之前**就先红一次。
#![allow(dead_code)]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rustls::pki_types::{CertificateDer, ServerName};
use xt_mitm::{LocalCa, ALPN_HTTP1};

/// 一个可编排的源站：读完整请求（头 + body），记下 body 原始字节，再回一段**原始响应字节**。
///
/// `responses` 按第几次连接依次取；只有一条时永远回它。
pub struct ScriptedOrigin {
    pub port: u16,
    pub hits: Arc<AtomicU64>,
    bodies: Arc<Mutex<Vec<Vec<u8>>>>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl ScriptedOrigin {
    pub fn start(responses: Vec<Vec<u8>>) -> Self {
        assert!(!responses.is_empty(), "源站至少要有一条应答");
        let listener = TcpListener::bind("127.0.0.1:0").expect("绑源站");
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let hits = Arc::new(AtomicU64::new(0));
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let responses = Arc::new(responses);
        let (h2, b2, s2) = (hits.clone(), bodies.clone(), stop.clone());
        let handle = std::thread::spawn(move || {
            let idx = AtomicU64::new(0);
            while !s2.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut sock, _)) => {
                        // 非阻塞监听 ⇒ accept 出来的已连接套接字继承非阻塞，必须设回阻塞。
                        let _ = sock.set_nonblocking(false);
                        let _ = sock.set_read_timeout(Some(Duration::from_secs(3)));
                        h2.fetch_add(1, Ordering::Relaxed);
                        if let Some(body) = origin_read_request(&mut sock) {
                            b2.lock().unwrap().push(body);
                        }
                        let i = idx.fetch_add(1, Ordering::Relaxed) as usize;
                        let last = responses.len().saturating_sub(1);
                        let resp = &responses[i.min(last)];
                        let _ = sock.write_all(resp);
                        let _ = sock.flush();
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5))
                    }
                    Err(_) => break,
                }
            }
        });
        Self { port, hits, bodies, stop, handle: Some(handle) }
    }

    pub fn hits(&self) -> u64 {
        self.hits.load(Ordering::Relaxed)
    }

    /// 等源站记下 `want` 条请求体（避免用 sleep 猜时间）。
    pub fn wait_bodies(&self, want: usize, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if self.bodies.lock().unwrap().len() >= want {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    pub fn last_body(&self) -> Vec<u8> {
        self.bodies.lock().unwrap().last().cloned().unwrap_or_default()
    }

    pub fn bodies(&self) -> Vec<Vec<u8>> {
        self.bodies.lock().unwrap().clone()
    }
}

impl Drop for ScriptedOrigin {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// 读一条完整请求，返回 **body 原始字节**（chunked 时含分块框架，逐字节原样）。
fn origin_read_request(sock: &mut TcpStream) -> Option<Vec<u8>> {
    let mut buf: Vec<u8> = Vec::new();
    let mut tmp = [0u8; 2048];
    let end = loop {
        if let Some(e) = head_end(&buf) {
            break e;
        }
        match sock.read(&mut tmp) {
            Ok(0) => return None,
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
            Err(_) => return None,
        }
    };
    let head = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
    let mut body = buf[end..].to_vec();
    if head.contains("transfer-encoding: chunked") {
        body = read_chunked_raw(sock, body);
    } else if let Some(n) = content_length(&head) {
        while body.len() < n {
            match sock.read(&mut tmp) {
                Ok(0) => break,
                Ok(k) => body.extend_from_slice(&tmp[..k]),
                Err(_) => break,
            }
        }
        body.truncate(n);
    }
    Some(body)
}

/// `mitm-upstream` 的替身：只说 SOCKS5，然后盲目搬运字节（忽略请求里的端口）。
pub struct FakeUpstream {
    pub port: u16,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl FakeUpstream {
    pub fn start(origin_port: u16) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("绑 socks");
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let s2 = stop.clone();
        let handle = std::thread::spawn(move || {
            while !s2.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((mut sock, _)) => {
                        let _ = sock.set_nonblocking(false);
                        let _ = sock.set_read_timeout(Some(Duration::from_secs(3)));
                        let mut hello = [0u8; 2];
                        if sock.read_exact(&mut hello).is_err() || sock.write_all(&[5, 0]).is_err() {
                            continue;
                        }
                        let mut fixed = [0u8; 4];
                        if sock.read_exact(&mut fixed).is_err() {
                            continue;
                        }
                        if fixed[3] == 3 {
                            let mut len = [0u8; 1];
                            let _ = sock.read_exact(&mut len);
                            let mut host = vec![0u8; len[0] as usize + 2];
                            let _ = sock.read_exact(&mut host);
                        }
                        if sock.write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0]).is_err() {
                            continue;
                        }
                        let Ok(mut out) = TcpStream::connect(("127.0.0.1", origin_port)) else {
                            continue;
                        };
                        let Ok(mut a2) = sock.try_clone() else { continue };
                        let Ok(mut b2) = out.try_clone() else { continue };
                        let t = std::thread::spawn(move || {
                            let _ = std::io::copy(&mut a2, &mut b2);
                        });
                        let _ = std::io::copy(&mut out, &mut sock);
                        let _ = t.join();
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5))
                    }
                    Err(_) => break,
                }
            }
        });
        Self { port, stop, handle: Some(handle) }
    }
}

impl Drop for FakeUpstream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// 进程级安装 rustls 的 crypto provider（幂等）。
pub fn install_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

pub fn client_config(ca_der: Vec<u8>) -> Arc<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(CertificateDer::from(ca_der)).expect("测试 CA 要能装进根仓库");
    let mut cfg = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    cfg.alpn_protocols = vec![ALPN_HTTP1.to_vec()];
    Arc::new(cfg)
}

/// 发一段**原始请求字节**，读回**原始响应字节**。返回 `(响应文本, 是否干净收尾)`。
///
/// 用原始字节而不是 `http` 客户端：判据 1/2 要比对的就是字节本身。
pub fn https_raw(
    proxy: SocketAddr,
    host: &str,
    request: &[u8],
    cfg: &Arc<rustls::ClientConfig>,
) -> (String, bool) {
    let tcp = TcpStream::connect(proxy).expect("连代理");
    tcp.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    tcp.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
    let name = ServerName::try_from(host.to_string()).expect("合法域名");
    let conn = rustls::ClientConnection::new(cfg.clone(), name).expect("客户端会话");
    let mut tls = rustls::StreamOwned::new(conn, tcp);
    tls.write_all(request).expect("写请求");
    tls.flush().unwrap();
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    let mut clean = false;
    loop {
        match tls.read(&mut buf) {
            Ok(0) => {
                clean = true;
                break;
            }
            Ok(n) => out.extend_from_slice(&buf[..n]),
            Err(_) => break,
        }
    }
    (String::from_utf8_lossy(&out).to_string(), clean)
}

/// 从 PEM 里取 CA 的 DER（只为测试；不引新依赖）。
pub fn ca_cert_der(ca: &LocalCa) -> Vec<u8> {
    let pem = ca.cert_pem();
    let body: String = pem
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .collect::<Vec<_>>()
        .join("");
    base64_decode(&body)
}

fn base64_decode(s: &str) -> Vec<u8> {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::new();
    let mut acc = 0u32;
    let mut bits = 0u32;
    for c in s.bytes() {
        if c == b'=' {
            break;
        }
        let Some(v) = T.iter().position(|t| *t == c) else { continue };
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

// ---- 极简 HTTP/1.1 解析（只为替身读写；不引新依赖）-----------------------------

pub fn head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4)
}

fn content_length(head_lower: &str) -> Option<usize> {
    head_lower.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim()
            .eq("content-length")
            .then(|| v.trim().parse::<usize>().ok())
            .flatten()
    })
}

fn find_crlf(buf: &[u8], from: usize) -> Option<usize> {
    if from >= buf.len() {
        return None;
    }
    buf[from..].windows(2).position(|w| w == b"\r\n").map(|p| p + from)
}

/// 读到**终止 chunk**（含 trailer 与最后的 CRLF），返回原样字节。
fn read_chunked_raw(sock: &mut TcpStream, mut buf: Vec<u8>) -> Vec<u8> {
    let mut pos = 0usize;
    let mut tmp = [0u8; 2048];
    loop {
        let line_end = loop {
            if let Some(i) = find_crlf(&buf, pos) {
                break i;
            }
            match sock.read(&mut tmp) {
                Ok(0) => return buf,
                Ok(n) => buf.extend_from_slice(&tmp[..n]),
                Err(_) => return buf,
            }
        };
        let token = buf[pos..line_end].split(|&b| b == b';').next().unwrap_or(&[]);
        let size = std::str::from_utf8(token)
            .ok()
            .and_then(|s| usize::from_str_radix(s.trim(), 16).ok())
            .unwrap_or(0);
        pos = line_end + 2;
        if size == 0 {
            loop {
                let le = loop {
                    if let Some(i) = find_crlf(&buf, pos) {
                        break i;
                    }
                    match sock.read(&mut tmp) {
                        Ok(0) => return buf,
                        Ok(n) => buf.extend_from_slice(&tmp[..n]),
                        Err(_) => return buf,
                    }
                };
                let empty = le == pos;
                pos = le + 2;
                if empty {
                    break;
                }
            }
            break;
        }
        let need = pos + size + 2;
        while buf.len() < need {
            match sock.read(&mut tmp) {
                Ok(0) => return buf,
                Ok(n) => buf.extend_from_slice(&tmp[..n]),
                Err(_) => return buf,
            }
        }
        pos = need;
    }
    buf.truncate(pos);
    buf
}
