//! 真核心验收：把 `generate()` 的产物交给**真实 Xray 二进制**做 `run -test -c`。
//!
//! 单元测试只能证明「我们写出的 JSON 是我们以为的那个形状」，
//! 证明不了「Xray 认这个形状」。字段名拼错时 serde_json 不会报错，
//! 只有核心自己会拒绝启动 —— 所以这一层验证不可省。
//!
//! 二进制路径：`XRAY_BIN` 环境变量，默认 `/Users/xbtg-/deepseek-harness/.scratch/bin/xray`。
//! **二进制不存在就失败**，不静默跳过：跳过会让「合法」这个结论失去依据。

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::process::Command;

use xt_contract::model::{LogLevel, NodeId};
use xt_xrayconf::{generate, generate_probe, ConfigInputs, OutboundSpec};

fn xray_bin() -> PathBuf {
    PathBuf::from(
        std::env::var("XRAY_BIN")
            .unwrap_or_else(|_| "/Users/xbtg-/deepseek-harness/.scratch/bin/xray".to_string()),
    )
}

/// 用 xt-subs 真解析一条链接，确保测的是「生产路径产出的 outbound」而不是手写夹具。
fn node_from_link(link: &str) -> (NodeId, serde_json::Value) {
    let nodes = xt_subs::parse_links(link).expect("解析测试链接");
    (nodes[0].id.clone(), nodes[0].outbound.clone())
}

fn assert_xray_accepts(config: &str, label: &str) {
    let bin = xray_bin();
    assert!(bin.exists(), "找不到 xray 二进制：{}（用 XRAY_BIN 指定）", bin.display());

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
