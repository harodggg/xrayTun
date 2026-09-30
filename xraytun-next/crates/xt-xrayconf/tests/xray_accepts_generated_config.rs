//! 真核心验收：把 `generate()` 的产物交给**真实 Xray 二进制**做 `run -test -c`。
//!
//! 单元测试只能证明「我们写出的 JSON 是我们以为的那个形状」，
//! 证明不了「Xray 认这个形状」。字段名拼错时 serde_json 不会报错，
//! 只有核心自己会拒绝启动 —— 所以这一层验证不可省。
//!
//! 二进制路径：`XT_XRAY_BIN`（或旧名 `XRAY_BIN`）→ PATH 里的 `xray`。
//! **不写死绝对路径**（那让 CI 必红，也不该出现在公开仓库里）。
//! **二进制不存在就失败**，不静默跳过：跳过会让「合法」这个结论失去依据。

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::process::Command;

use xt_contract::model::{LogLevel, NodeId};
use xt_xrayconf::{generate, generate_probe, ConfigInputs, OutboundSpec};

fn xray_bin() -> PathBuf {
    for key in ["XT_XRAY_BIN", "XRAY_BIN"] {
        if let Ok(path) = std::env::var(key) {
            return PathBuf::from(path);
        }
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join("xray");
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    panic!(
        "这个测试要把生成的配置交给真核心做 `xray run -test -c` 校验，需要真 xray 二进制：\
         设置 XT_XRAY_BIN=/path/to/xray，或把它放进 PATH。"
    );
}

/// 用 xt-subs 真解析一条链接，确保测的是「生产路径产出的 outbound」而不是手写夹具。
fn node_from_link(link: &str) -> (NodeId, serde_json::Value) {
    let nodes = xt_subs::parse_links(link).expect("解析测试链接");
    (nodes[0].id.clone(), nodes[0].outbound.clone())
}

fn assert_xray_accepts(config: &str, label: &str) {
    // `xray_bin()` 已经保证"要么拿到真实路径，要么带着怎么配置的提示 panic"，
    // 所以这里不再多写一条 exists 断言（那条会让人误以为还有第二种给法）。
    let bin = xray_bin();

    let path = std::env::temp_dir().join(format!("xt-xrayconf-{}-{label}.json", std::process::id()));
    std::fs::write(&path, config).expect("写临时配置");

    let output = Command::new(&bin)
        .arg("run")
        .arg("-test")
        .arg("-c")
        .arg(&path)
        .output()
        .expect("执行 xray");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "xray run -test 拒绝了我们生成的配置（{label}，退出码 {:?}）\n配置文件: {}\nstdout:\n{stdout}\nstderr:\n{stderr}",
        output.status.code(),
        path.display()
    );
    eprintln!("[{label}] xray -test 通过：{}", stdout.trim());
    let _ = std::fs::remove_file(&path);
}

/// 主实例：socks + api + vless 节点 + direct。
#[test]
fn real_xray_accepts_the_proxy_config() {
    let (id, outbound) = node_from_link(
        "vless://11111111-1111-1111-1111-111111111111@127.0.0.1:17443?encryption=none&security=none&type=tcp#E2E",
    );
    let inputs = ConfigInputs {
        listen_socks: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 17580),
        api_listen: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 17581),
        selected: OutboundSpec { node_id: id, outbound },
        log_level: LogLevel::Info,
    };
    let config = generate(&inputs).expect("生成配置");
    assert_xray_accepts(&config, "proxy");
}

/// 探测实例：多个独立 socks 入站 + 各自的 outbound。
#[test]
fn real_xray_accepts_the_probe_config() {
    let (id_a, outbound_a) = node_from_link(
        "vless://11111111-1111-1111-1111-111111111111@127.0.0.1:17443?encryption=none&security=none#A",
    );
    let (id_b, outbound_b) = node_from_link(
        "trojan://password@127.0.0.1:17444?security=none&type=tcp#B",
    );
    let specs = vec![
        OutboundSpec { node_id: id_a, outbound: outbound_a },
        OutboundSpec { node_id: id_b, outbound: outbound_b },
    ];
    let (config, ports) = generate_probe(&specs, 17600).expect("生成探测配置");
    assert_eq!(ports, vec![17600, 17601]);
    assert_xray_accepts(&config, "probe");
}

/// REALITY + gRPC 节点：最容易写错字段名的一种组合。
#[test]
fn real_xray_accepts_a_reality_grpc_node() {
    let (id, outbound) = node_from_link(
        "vless://11111111-1111-1111-1111-111111111111@127.0.0.1:17445?encryption=none&security=reality&sni=www.microsoft.com&fp=chrome&pbk=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA&sid=0123456789abcdef&type=grpc&serviceName=grpcsvc#REALITY",
    );
    let inputs = ConfigInputs {
        listen_socks: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 17582),
        api_listen: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 17583),
        selected: OutboundSpec { node_id: id, outbound },
        log_level: LogLevel::Debug,
    };
    assert_xray_accepts(&generate(&inputs).expect("生成配置"), "reality-grpc");
}
