import { useState } from "react";
import { api } from "../ipc";
import { useStore } from "../store";
import {
  DNS_MODE_LABEL,
  IPV6_LABEL,
  MODE_LABEL,
  type AppSettings,
  type DnsHandling,
  type Ipv6Mode,
} from "../types";

const HELPER_STATE_LABEL: Record<string, string> = {
  ready: "已就绪",
  not_installed: "未安装",
  not_running: "已安装但进程未运行",
  not_permitted: "权限不足",
  needs_approval: "等待系统批准",
  unknown: "状态未知",
};

function helperStateLabel(state: string, version: string | null, protocol: number | null): string {
  const base = HELPER_STATE_LABEL[state] ?? state;
  if (state === "ready") {
    return `${base}（版本 ${version ?? "?"}，协议 v${protocol ?? "?"}）`;
  }
  return base;
}

export default function Settings() {
  const { snapshot, busy, run, runVoid } = useStore();
  const [draft, setDraft] = useState<AppSettings | null>(null);

  if (!snapshot) return <div className="empty">正在加载…</div>;

  const settings = draft ?? snapshot.settings;
  const dirty = draft !== null;

  const patch = (p: Partial<AppSettings>) => setDraft({ ...settings, ...p });
  const patchTun = (p: Partial<AppSettings["tun"]>) => patch({ tun: { ...settings.tun, ...p } });
  const patchDns = (p: Partial<AppSettings["dns"]>) => patch({ dns: { ...settings.dns, ...p } });
  const patchFake = (p: Partial<AppSettings["fakedns"]>) =>
    patch({ fakedns: { ...settings.fakedns, ...p } });

  const save = async () => {
    if (!dirty) return;
    const ok = await run("save", () => api.saveSettings(settings));
    if (ok) setDraft(null);
  };

  const restart = async () => {
    if (dirty) await save();
    await run("restart", async () => {
      await api.stop();
      return api.start();
    });
  };

  return (
    <>
      {dirty && (
        <div className="banner banner--warn">
          <span>✎</span>
          <div style={{ flex: 1 }}>有未保存的改动。</div>
          <button className="btn" onClick={() => setDraft(null)}>
            放弃
          </button>
          <button className="btn btn--primary" disabled={busy !== null} onClick={() => void save()}>
            保存
          </button>
        </div>
      )}

      {/* ------------------------------------------------------- 代理入口 */}
      <section className="card">
        <h2 className="card__title">代理入口</h2>
        <p className="card__desc">
          SOCKS 入站的 UDP 支持是 TUN 模式和 QUIC 转发的必要条件，因此始终开启。
          端口低于 1024 会被拒绝 —— 那是特权端口，与 helper 的最小权限设计冲突。
        </p>
        <div className="grid-2">
          <div className="field">
            <label>SOCKS5 端口</label>
            <input
              type="number"
              min={1024}
              max={65535}
              value={settings.socks_port}
              onChange={(e) => patch({ socks_port: Number(e.target.value) })}
            />
          </div>
          <div className="field">
            <label>HTTP 端口</label>
            <input
              type="number"
              min={1024}
              max={65535}
              value={settings.http_port}
              onChange={(e) => patch({ http_port: Number(e.target.value) })}
            />
          </div>
        </div>
        <label className="row" style={{ gap: 8, fontSize: 12 }}>
          <input
            type="checkbox"
            checked={settings.allow_lan}
            onChange={(e) => patch({ allow_lan: e.target.checked })}
          />
          允许局域网设备使用本机代理（会监听 0.0.0.0，请确认网络环境可信）
        </label>
        <div className="field" style={{ marginTop: 14 }}>
          <label>日志级别</label>
          <select value={settings.log_level} onChange={(e) => patch({ log_level: e.target.value })}>
            <option value="silent">silent</option>
            <option value="error">error</option>
            <option value="warning">warning</option>
            <option value="info">info</option>
            <option value="debug">debug</option>
          </select>
          <div className="field__hint">
            debug 会产生大量日志。排查连接问题时临时打开，用完记得调回去。
          </div>
        </div>
      </section>

      {/* ------------------------------------------------------- TUN */}
      <section className="card">
        <h2 className="card__title">TUN 模式</h2>
        <p className="card__desc">
          当前模式：<strong>{MODE_LABEL[settings.mode]}</strong>。
          TUN 通过 Xray 原生的 tun 入站（内置 gVisor 协议栈）接管系统流量，
          由特权 helper 负责建网卡、装路由与改 DNS。
        </p>

        <div className="grid-2">
          <div className="field">
            <label>隧道网段</label>
            <input
              type="text"
              value={settings.tun.network}
              onChange={(e) => patchTun({ network: e.target.value })}
            />
            <div className="field__hint">
              默认 <span className="mono">198.18.0.1/15</span>（RFC 2544 保留段，公网不可路由）。
              <strong>主机位会被保留</strong> —— 这里的 .1 是接口自己的地址，不是网络地址。
            </div>
          </div>
          <div className="field">
            <label>MTU</label>
            <input
              type="number"
              min={576}
              max={9000}
              value={settings.tun.mtu}
              onChange={(e) => patchTun({ mtu: Number(e.target.value) })}
            />
            <div className="field__hint">
              默认 1500。出现「大文件下载卡住但网页能开」时可以试 1400。
            </div>
          </div>
          <div className="field">
            <label>哨兵 DNS</label>
            <input
              type="text"
              value={settings.tun.sentinel_dns}
              onChange={(e) => patchTun({ sentinel_dns: e.target.value })}
            />
            <div className="field__hint">
              写入系统的「假」解析器，位于隧道网段内且不会被真实路由 —— 唯一目的是让所有
              53 端口流量必然进入隧道，从而被内核的 <span className="mono">dns-out</span> 接管。
            </div>
          </div>
          <div className="field">
            <label>IPv6 处理</label>
            <select
              value={settings.tun.ipv6}
              onChange={(e) => patchTun({ ipv6: e.target.value as Ipv6Mode })}
            >
              {(Object.keys(IPV6_LABEL) as Ipv6Mode[]).map((k) => (
                <option key={k} value={k}>
                  {IPV6_LABEL[k]}
                </option>
              ))}
            </select>
            <div className="field__hint">
              选「不接管」时，IPv6 流量会绕过隧道走物理网卡 —— 可能泄漏真实出口，
              但不会因为内核 IPv6 配置问题导致断网。这是一个刻意的取舍。
            </div>
          </div>
        </div>

        <div className="field">
          <label>出站绑定接口（防路由环）</label>
          <input
            type="text"
            placeholder="留空 = 自动使用物理出口（推荐）"
            value={settings.tun.bind_outbound_to ?? ""}
            onChange={(e) => patchTun({ bind_outbound_to: e.target.value.trim() || null })}
          />
          <div className="field__hint">
            填入 <span className="mono">en0</span> 这类物理接口名后，核心会用
            <span className="mono"> IP_BOUND_IF </span>
            把「连代理服务器」的 socket 绑到该接口，从根上杜绝
            「代理流量又被送进隧道」的路由环。留空时由核心自动探测。
          </div>
        </div>

        <label className="row" style={{ gap: 8, fontSize: 12 }}>
          <input
            type="checkbox"
            checked={settings.tun.bypass_private}
            onChange={(e) => patchTun({ bypass_private: e.target.checked })}
          />
          把内网 / 链路本地 / 多播地址排除在隧道之外（强烈建议保持开启）
        </label>
      </section>

      {/* ------------------------------------------------------- DNS */}
      <section className="card">
        <h2 className="card__title">DNS</h2>
        <p className="card__desc">
          macOS 的 DNS 是<strong>按网络服务</strong>配置的。helper 会在改动前备份、在回滚时还原 ——
          如果不还原，「代理关掉之后所有网站都打不开」是这类工具最常见的差评来源。
        </p>
        <div className="field">
          <label>解析策略</label>
          <select
            value={settings.dns.mode}
            onChange={(e) => patchDns({ mode: e.target.value as DnsHandling })}
          >
            {(Object.keys(DNS_MODE_LABEL) as DnsHandling[]).map((k) => (
              <option key={k} value={k}>
                {DNS_MODE_LABEL[k]}
              </option>
            ))}
          </select>
        </div>
        <div className="grid-2">
          <div className="field">
            <label>远端 DNS（走代理）</label>
            <textarea
              rows={3}
              value={settings.dns.remote_servers.join("\n")}
              onChange={(e) =>
                patchDns({ remote_servers: e.target.value.split("\n").map((s) => s.trim()).filter(Boolean) })
              }
            />
            <div className="field__hint">每行一个，支持 https:// / tls:// / 纯 IP。</div>
          </div>
          <div className="field">
            <label>本地 DNS（直连）</label>
            <textarea
              rows={3}
              value={settings.dns.direct_servers.join("\n")}
              onChange={(e) =>
                patchDns({ direct_servers: e.target.value.split("\n").map((s) => s.trim()).filter(Boolean) })
              }
            />
          </div>
        </div>
        <label className="row" style={{ gap: 8, fontSize: 12 }}>
          <input
            type="checkbox"
            checked={settings.dns.sniffing}
            onChange={(e) => patchDns({ sniffing: e.target.checked })}
          />
          开启流量嗅探（按 SNI / Host 分流）
        </label>
        <div className="field__hint" style={{ marginTop: 6 }}>
          关闭嗅探后，域名分流只能依赖 DNS 阶段的信息，对「直接用 IP 发起连接」的程序会失效。
        </div>
      </section>

      {/* ------------------------------------------------------- Fake-IP */}
      <section className="card">
        <h2 className="card__title">Fake-IP</h2>
        <p className="card__desc">
          很多人以为 Fake-IP 是 sing-box 独有 —— <strong>不是</strong>。Xray 有原生的
          <span className="mono"> fakedns </span>配置段，配合
          <span className="mono"> destOverride: ["fakedns+others"] </span>
          的语义与 sing-box 的 fakeip + sniffing 等价。
        </p>
        <label className="row" style={{ gap: 8, fontSize: 12 }}>
          <input
            type="checkbox"
            checked={settings.fakedns.enabled}
            onChange={(e) => patchFake({ enabled: e.target.checked })}
          />
          启用 Fake-IP
        </label>
        {settings.fakedns.enabled && (
          <>
            <div className="banner banner--warn" style={{ marginTop: 12 }}>
              <span>⚠︎</span>
              <div>
                Fake-IP 会污染本机 DNS 缓存。隧道关闭后的一段时间内可能出现「网络无法访问」，
                需要等缓存过期或手动刷新（<span className="mono">sudo killall -HUP mDNSResponder</span>）。
                因此默认关闭。
              </div>
            </div>
            <div className="grid-2" style={{ marginTop: 12 }}>
              <div className="field">
                <label>地址池</label>
                <input
                  type="text"
                  value={settings.fakedns.ip_pool}
                  onChange={(e) => patchFake({ ip_pool: e.target.value })}
                />
              </div>
              <div className="field">
                <label>池大小</label>
                <input
                  type="number"
                  value={settings.fakedns.pool_size}
                  onChange={(e) => patchFake({ pool_size: Number(e.target.value) })}
                />
              </div>
            </div>
          </>
        )}
      </section>

      {/* ------------------------------------------------------- 内核 */}
      <section className="card">
        <h2 className="card__title">内核</h2>
        <div className="field">
          <label>Xray 可执行文件路径</label>
          <input
            type="text"
            placeholder={snapshot.core.path ?? "留空 = 自动查找"}
            value={settings.core_path ?? ""}
            onChange={(e) => patch({ core_path: e.target.value.trim() || null })}
          />
          <div className="field__hint">
            当前：{snapshot.core.path ?? snapshot.core.error ?? "未找到"}
            {snapshot.core.version ? ` · ${snapshot.core.version}` : ""}
            <br />
            TUN 模式需要 <span className="mono">&gt;= {snapshot.core.min_native_tun_version}</span>
            （该版本才补齐了 macOS 上的地址与路由编程）。建议使用最新的 26.9.x。
          </div>
        </div>
        <div className="row">
          <button className="btn" disabled={busy !== null} onClick={() => void restart()}>
            {busy === "restart" ? <span className="spin" /> : null}
            保存并重启核心
          </button>
        </div>
      </section>

      {/* ------------------------------------------------------- helper */}
      <section className="card">
        <h2 className="card__title">特权助手（helper）</h2>
        <p className="card__desc">
          macOS 上创建 utun 必须具备 root 权限，而把整个界面跑在 root 下是不可接受的。
          因此把「建网卡、装路由、改 DNS」这三件事拆成一个只做这些事的 root 守护进程：
          它不认识任何代理协议，也不读你的订阅，即使界面被攻破也只能改本机网络配置。
        </p>
        <dl className="kv" style={{ marginBottom: 14 }}>
          <dt>状态</dt>
          <dd>{helperStateLabel(snapshot.helper.state, snapshot.helper.version, snapshot.helper.protocol)}</dd>
          <dt>socket</dt>
          <dd className="mono">{"/var/run/com.xraytun.helper.sock"}</dd>
          <dt>遗留会话</dt>
          <dd>{snapshot.helper.stale_session ?? "无"}</dd>
        </dl>

        {snapshot.helper.state !== "ready" && snapshot.helper.error && (
          <div
            className={`banner ${
              snapshot.helper.state === "not_installed" ? "banner--info" : "banner--warn"
            }`}
            style={{ whiteSpace: "pre-wrap" }}
          >
            <span>ℹ︎</span>
            <div>{snapshot.helper.error}</div>
          </div>
        )}
        <div className="row row--wrap">
          <button
            className="btn btn--primary"
            disabled={busy !== null}
            onClick={() => void run("install-helper", () => api.installHelper())}
          >
            {busy === "install-helper" ? <span className="spin" /> : null}
            {snapshot.helper.state === "not_installed" ? "安装 helper" : "重新安装 helper"}
          </button>
          {snapshot.helper.state === "not_running" && (
            <button
              className="btn btn--primary"
              disabled={busy !== null}
              onClick={() => void run("restart-helper", () => api.restartHelper())}
            >
              {busy === "restart-helper" ? <span className="spin" /> : null}
              重启 helper
            </button>
          )}
          <button
            className="btn"
            disabled={busy !== null}
            onClick={() => void run("restore", () => api.restoreStale())}
          >
            修复网络（回滚遗留配置）
          </button>
          <button
            className="btn btn--danger"
            disabled={busy !== null}
            onClick={() => void run("uninstall-helper", () => api.uninstallHelper())}
          >
            卸载 helper
          </button>
        </div>
        <div className="field__hint" style={{ marginTop: 10 }}>
          安装会写入 <span className="mono">/Library/LaunchDaemons</span> 与
          <span className="mono"> /Library/PrivilegedHelperTools</span>，需要一次管理员授权。
          发行版应改用 <span className="mono">SMAppService</span>（macOS 13+）：
          无需密码，但用户需要在「系统设置 → 通用 → 登录项与扩展 → 后台允许」里启用。
        </div>
      </section>

      {/* ------------------------------------------------- DNS 解析器 */}
      <section className="card">
        <h2 className="card__title">DNS 解析器</h2>

        <label className="row" style={{ gap: 8, fontSize: 12, marginBottom: 10 }}>
          <input
            type="checkbox"
            checked={settings.dns.auto_select}
            onChange={(e) => patchDns({ auto_select: e.target.checked } as Partial<typeof settings.dns>)}
          />
          启动时自动检测可用的解析器，把最快的排到前面
        </label>

        <div className="row row--wrap" style={{ gap: 8 }}>
          <button className="btn" disabled={busy !== null}
                  onClick={() => void run("probe-dns", () => api.probeDns())}>
            立即检测
          </button>
        </div>

        {/* 两组分开显示：它们是两条**不同的测量路径**，混在一张表里会被误读成
            同一把尺子量出来的数字。 */}
        {(["domestic", "foreign"] as const).map((kind) => {
          const rows = snapshot.dns.probes.filter((p) => p.kind === kind);
          if (rows.length === 0) return null;
          const isCn = kind === "domestic";
          const chosen = isCn ? snapshot.dns.chosen : snapshot.dns.chosen_foreign;
          const err = isCn ? snapshot.dns.error : snapshot.dns.foreign_error;
          return (
            <div key={kind} className="probe-group">
              <div className="probe-group__title">
                {isCn ? "国内解析器 · 直连测量" : "国外解析器 · 经节点测量"}
                <span className="field__hint">
                  {isCn ? "用于 geosite:cn → direct_servers" : "用于 geosite:geolocation-!cn → remote_servers"}
                </span>
              </div>

              {err && (
                <div className="banner banner--warn" style={{ marginTop: 6 }}>
                  <span>⚠︎</span><div>{err}</div>
                </div>
              )}

              {chosen && (
                <div className="field__hint" style={{ margin: "6px 0 0" }}>
                  当前首选 <span className="mono">{chosen}</span>
                </div>
              )}

              <div className="probe-table">
                {rows.map((p) => (
                  <div key={p.server} className="probe-table__row">
                    <span className="mono">{p.server}</span>
                    <span className="field__hint">{p.label}</span>
                    <span className={`badge badge--${p.suspect || !p.answered ? "slow" : "fast"}`}>
                      {p.latency_ms !== null ? `${p.latency_ms} ms` : p.note ? "未探测" : "不通"}
                    </span>
                    {p.suspect && <span className="field__hint">与同组多数派不一致</span>}
                    {p.note && <span className="field__hint">{p.note}</span>}
                  </div>
                ))}
              </div>
            </div>
          );
        })}

        <div className="field__hint" style={{ marginTop: 12 }}>
          两组走两条不同的路径，因为在配置里它们本来就用得不一样：
          <br />
          <span className="mono">国内</span> 是明文 UDP，测的时候把 socket 绑到物理网卡
          绕过隧道 —— 不绑的话并发探测测到的是核心排队而不是解析器延迟。
          <br />
          <span className="mono">国外</span> 只有经节点才连得上（直连
          <span className="mono"> 1.1.1.1:443 </span>实测 8 秒超时），所以经本地 SOCKS
          入站去测 —— 那也正是它在分流规则里被使用时的路径。没连接节点时这一组显示
          「未探测」，而不是拿直连的超时冒充「都不通」。
          <br />
          两组各自还要判「答得对不对」：让同组所有解析器查同一个域名，答案跟组内多数派
          不一致的标为可疑（明文入墙会被抢答，抢答者延迟一定漂亮、答案却可能是错的）。
          国外那组走的是加密 DoH，基本不可能被抢答。
        </div>
      </section>

      {/* --------------------------------------------- 核心与 geo 更新 */}
      <section className="card">
        <h2 className="card__title">核心与数据更新</h2>

        <div className="kv">
          <div>
            <div className="kv__k">客户端</div>
            <div className="kv__v mono">v{snapshot.app_version}</div>
          </div>
          <div>
            <div className="kv__k">核心</div>
            <div className="kv__v mono">
              {snapshot.update.core_version ?? "未找到"}
              {snapshot.update.core_managed && (
                <span className="field__hint" style={{ marginLeft: 6 }}>（更新版）</span>
              )}
            </div>
          </div>
          <div>
            <div className="kv__k">geo 数据</div>
            <div className="kv__v mono">{snapshot.update.geo_tag ?? "随包附带"}</div>
          </div>
        </div>

        {/* 两条通道分开：核心几个月一次，geo 数据上游每天更新。
            合成一个「检查更新」会让用户以为必须一起升级。 */}
        <div className="row row--wrap" style={{ marginTop: 12 }}>
          <button className="btn" disabled={busy !== null}
                  onClick={() => void run("check-updates", () => api.checkUpdates())}>
            检查更新
          </button>
          {snapshot.update.latest_core && (
            <button className="btn btn--primary" disabled={busy !== null}
                    onClick={() => void run("install-core", () => api.installCoreUpdate())}>
              更新核心到 {snapshot.update.latest_core.version}
              {snapshot.update.latest_core.prerelease ? "（预发布）" : ""}
            </button>
          )}
          {snapshot.update.latest_geo && (
            <button className="btn btn--primary" disabled={busy !== null}
                    onClick={() => void run("install-geo", () => api.installGeoUpdate())}>
              更新 geo 到 {snapshot.update.latest_geo.version}
            </button>
          )}
          {snapshot.update.core_managed && (
            <button className="btn btn--ghost" disabled={busy !== null}
                    onClick={() => void run("revert-update", () => api.revertManagedUpdate())}>
              回退到随包版本
            </button>
          )}
        </div>

        {snapshot.update.check_error && (
          <div className="banner banner--warn" style={{ marginTop: 10 }}>
            <span>⚠︎</span>
            <div>检查更新失败：{snapshot.update.check_error}</div>
          </div>
        )}

        <div className="field__hint" style={{ marginTop: 10 }}>
          更新装在数据目录里，**不会改动 App 包本身**（改包内文件会让签名失效），
          所以「回退到随包版本」就是删掉那些文件，永远可用。
          装上后需要重新连接才会生效。
          <br />
          核心的新版本在 GitHub 上全部标为「预发布」，所以这里如实标出 ——
          按 GitHub 的 <span className="mono">latest</span> 判断会把你降到几个月前的旧版。
        </div>

        {/* ------------------------------------- 客户端自身更新 */}
        <div style={{ marginTop: 22, paddingTop: 16, borderTop: "1px solid var(--border)" }}>
          <div className="kv" style={{ marginBottom: 10 }}>
            <div>
              <div className="kv__k">客户端</div>
              <div className="kv__v mono">{snapshot.app_version}</div>
            </div>
            <div>
              <div className="kv__k">GitHub 上的最新版</div>
              <div className="kv__v mono">{snapshot.update.latest_app?.version ?? "—"}</div>
            </div>
          </div>

          <div className="row row--wrap" style={{ gap: 8 }}>
            <button className="btn" disabled={busy !== null}
                    onClick={() => void run("check-app", () => api.checkAppUpdate())}>
              检查客户端更新
            </button>
            {snapshot.update.latest_app && (
              <button className="btn btn--primary" disabled={busy !== null}
                      onClick={() => void run("install-app", () => api.installAppUpdate())}>
                更新到 {snapshot.update.latest_app.version} 并重启
              </button>
            )}
          </div>

          <label className="field" style={{ marginTop: 12 }}>
            <span className="field__label">GitHub token（只读）</span>
            <input
              className="input mono"
              type="password"
              placeholder="ghp_… 或 github_pat_…"
              value={settings.github_token}
              onChange={(e) => patch({ github_token: e.target.value } as Partial<typeof settings>)}
            />
          </label>

          <div className="field__hint" style={{ marginTop: 8 }}>
            客户端仓库是<span className="mono">私有</span>的，GitHub 对未认证的私有仓库
            请求一律返回 404（实测），所以不给 token 就<b>永远收不到更新</b> ——
            这一步不是可选项。填一个 fine-grained token、只勾这一个仓库的
            <span className="mono"> Contents: Read </span>即可，不要给写权限。
            仓库改成公开之后这里可以留空。
            <br />
            <b>安装会在替换 App 之后自动重启。</b>更新脚本先等你退出、再替换
            <span className="mono"> /Applications/XrayTun.app</span>，所以安装前请先
            断开隧道。校验只用 release 里的 <span className="mono">SHA256SUMS.txt</span>
            （能防下载损坏，<b>防不了上游被换掉</b> —— 那需要签名，而这个包是 ad-hoc 签名），
            日志在 <span className="mono">~/Library/Logs/XrayTun/app-update.log</span>。
          </div>
        </div>
      </section>

      {/* ------------------------------------------------------- 杂项 */}
      <section className="card">
        <h2 className="card__title">其他</h2>
        <label className="row" style={{ gap: 8, fontSize: 12, marginBottom: 10 }}>
          <input
            type="checkbox"
            // 勾选状态来自**系统**（snapshot.login_item），不是 settings 字段。
            // 用户可以在「系统设置 → 通用 → 登录项」里直接删掉这一项，
            // 那种情况下 settings 说「开着」而现实是「不会自启」。
            checked={snapshot.login_item.status === "enabled" ||
              snapshot.login_item.status === "requires_approval"}
            disabled={busy === "login-item"}
            onChange={(e) =>
              void run("login-item", () => api.setLaunchAtLogin(e.target.checked))
            }
          />
          开机自启动
          <span className="field__hint" style={{ marginLeft: 6 }}>
            {snapshot.login_item.detail}
          </span>
        </label>
        {snapshot.login_item.needs_approval && (
          <div className="banner banner--warn" style={{ marginBottom: 10 }}>
            <span>⚠︎</span>
            <div style={{ flex: 1 }}>
              系统已登记，但还需要你在「系统设置 → 通用 → 登录项与扩展」里允许它。
            </div>
            <button
              className="btn btn--ghost"
              onClick={() => void runVoid("open-login-items", () => api.openLoginItemSettings())}
            >
              打开设置
            </button>
          </div>
        )}

        <label className="row" style={{ gap: 8, fontSize: 12, marginBottom: 10 }}>
          <input
            type="checkbox"
            checked={settings.show_speed_in_title}
            onChange={(e) => patch({ show_speed_in_title: e.target.checked })}
          />
          在标题栏与菜单栏显示实时网速
        </label>
        <div className="field__hint" style={{ marginBottom: 10 }}>
          窗口标题栏显示 <span className="mono">↓ 1.2 MB/s ↑ 34 KB/s</span>；
          菜单栏因为要和系统图标抢地方，用更短的 <span className="mono">↓1.2M ↑34K</span>，
          且空闲时不显示。速率来自核心的流量计数器，系统代理与 TUN 两种模式都有效。
        </div>
        <label className="row" style={{ gap: 8, fontSize: 12, marginBottom: 10 }}>
          <input
            type="checkbox"
            checked={settings.restore_system_proxy_on_exit}
            onChange={(e) => patch({ restore_system_proxy_on_exit: e.target.checked })}
          />
          退出时还原系统代理设置
        </label>
        <div className="row row--wrap">
          <button className="btn" onClick={() => void runVoid("open-dir", () => api.openDataDir())}>
            打开数据目录
          </button>
          <button
            className="btn"
            disabled={busy !== null}
            onClick={async () => {
              const text = await api.diagnostics();
              await navigator.clipboard.writeText(text).catch(() => console.log(text));
            }}
          >
            复制诊断报告
          </button>
        </div>
        <div className="field__hint" style={{ marginTop: 10 }}>
          数据目录：<span className="mono">{snapshot.runtime.config_path?.replace(/\/runtime\/.*$/, "") ?? "~/Library/Application Support/com.xraytun.desktop"}</span>
        </div>
      </section>
    </>
  );
}
