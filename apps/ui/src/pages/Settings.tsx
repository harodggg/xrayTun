import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { api } from "../ipc";
import { InlineConfirm } from "../InlineConfirm";
import { useStore } from "../store";
import {
  DNS_MODE_LABEL,
  IPV6_LABEL,
  MODE_LABEL,
  UpdateProgress,
  type AppSettings,
  type AppSnapshot,
  type DnsHandling,
  type Ipv6Mode,
} from "../types";

/**
 * 设置页的两级结构：**分类 → 分节**（task-48）。
 *
 * ## 为什么分这四类
 *
 * 与用户的**任务顺序**一致，也与旧导航的四个词对齐（连接 / DNS / 内核与更新 / 系统与助手）：
 * 先把流量接进来（代理入口、TUN、Fake-IP）→ 再管域名解析（DNS、解析器）→ 再管核心与数据更新
 * → 最后是系统集成（开机自启动、特权助手、其他）。
 *
 * ## 它是**唯一真源**
 *
 * 导航、深链解析（`#set-helper`）、跨页意图（`onNavigate("settings", "set-helper")`）、
 * 分类上的注意力徽标，全部从这张表读。所以「10 个分节一个不漏、不重」只需要在这张表里
 * 成立一次，并由测试逐项断言（`ALL_SETTINGS_SECTIONS`）。
 */
export const SETTINGS_CATEGORIES = [
  {
    id: "conn",
    label: "连接",
    sections: [
      { id: "set-entry", title: "代理入口" },
      { id: "set-tun", title: "TUN 模式" },
      { id: "set-fakeip", title: "Fake-IP" },
    ],
  },
  {
    id: "dns",
    label: "DNS",
    sections: [
      { id: "set-dns", title: "DNS" },
      { id: "set-dns-probe", title: "DNS 解析器" },
    ],
  },
  {
    id: "core",
    label: "内核与更新",
    sections: [
      { id: "set-core", title: "内核" },
      { id: "set-update", title: "核心与数据更新" },
    ],
  },
  {
    id: "system",
    label: "系统与助手",
    sections: [
      { id: "set-autostart", title: "开机自启动" },
      { id: "set-helper", title: "特权助手" },
      { id: "set-misc", title: "其他" },
    ],
  },
] as const;

export type SettingsCategoryId = (typeof SETTINGS_CATEGORIES)[number]["id"];

/** 全部 10 个分节 id（顺序 = 表里的顺序）。测试用它断言「不漏不重」。 */
export const ALL_SETTINGS_SECTIONS: string[] = SETTINGS_CATEGORIES.flatMap((c) =>
  c.sections.map((s) => s.id),
);

/**
 * 分节 id → 分类 id。
 *
 * **深链（`#set-helper`）与跨页意图（`onNavigate("settings", "set-helper")`）共用这一处解析** ——
 * 不许各写一份，否则两级结构一旦调整，两条入口就会有一条落错分类。
 * 这不是猜测：`Dashboard.tsx` 里「helper 没就绪 → 把用户送去设置页」正需要它，
 * 落错分类等于让用户去找一个看不见的东西。
 */
export function categoryOfSection(sectionId: string): SettingsCategoryId | null {
  for (const c of SETTINGS_CATEGORIES) {
    if (c.sections.some((s) => s.id === sectionId)) return c.id;
  }
  return null;
}

/** 当前 hash 指向的分节（`#set-helper` → `set-helper`）。 */
function hashSection(): string | null {
  const raw = window.location.hash.replace(/^#/, "");
  return raw ? raw : null;
}

/**
 * 每个分类里有没有**需要用户处理**的状态。
 *
 * 两级之后最危险的新问题就是「把该看见的东西藏起来」：某分类里的失败状态如果只能切过去
 * 才看得到，那就是新的「看不见的问题」。所以这里算出来给**分类标签**当徽标用
 * （选择「标签给指示」而不是「自动选中出问题的分类」：后者会在用户没要求时
 * 把内容换掉，属于静默跳转；徽标则是不打扰的常驻提示）。
 *
 * 只认**明确的失败/未就绪**：读数缺失（例如刚启动还没查更新）不算「有事」——
 * 不把「不知道」说成「有问题」。
 */
export function categoryAttention(snapshot: AppSnapshot): Record<SettingsCategoryId, boolean> {
  const helperDown = !snapshot.helper.socket_present || !snapshot.helper.reachable;
  return {
    // TUN 模式依赖 helper：正在用 TUN 而 helper 没就绪 → 连接这一类里确有事要处理
    conn: snapshot.settings.mode === "tun" && helperDown,
    dns: Boolean(snapshot.dns.error || snapshot.dns.foreign_error),
    core: Boolean(snapshot.core.error || snapshot.update.check_error),
    system: helperDown,
  };
}

/** 上次看过的分类（会话级，见 `selectCategory` 的理由）。 */
const LAST_CATEGORY_KEY = "xraytun.settings.category";

/** 落地时该选哪一类：**明确目标优先** → 上次看过 → 默认第一类。 */
function resolveInitialCategory(target: string | null | undefined): SettingsCategoryId {
  const fromTarget = target ? categoryOfSection(target) : null;
  if (fromTarget) return fromTarget;
  try {
    const saved = sessionStorage.getItem(LAST_CATEGORY_KEY);
    if (saved && SETTINGS_CATEGORIES.some((c) => c.id === saved)) return saved as SettingsCategoryId;
  } catch {
    /* 隐私模式拿不到 sessionStorage：用默认分类 */
  }
  return SETTINGS_CATEGORIES[0].id;
}

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

/**
 * 一个分节。**只渲染当前分类里的分节** —— 其他分类的分节**不在 DOM 里**。
 *
 * 为什么不是「用 CSS 藏起来」：藏起来的东西照样在 a11y 树里可被读屏读到、也可能被
 * Tab 聚焦到，而且「当前页面里到底有什么」这件事没法用 DOM 断言。不渲染就没有这些歧义。
 */
function Section({
  id,
  active,
  children,
}: {
  id: string;
  active: boolean;
  children: ReactNode;
}) {
  if (!active) return null;
  return (
    <section className="card set__sec" id={id}>
      {children}
    </section>
  );
}

/**
 * `auto_reconnect`：**后端有、前端类型没有**。
 *
 * `crates/xt-core/src/model.rs` 里它是 `#[serde(default = "yes")] pub auto_reconnect: bool`
 * （默认 `true`），启动时由 `commands/core.rs` 的 `should_auto_reconnect` 决定要不要连回来；
 * 但 `types.ts` 的 `AppSettings` 从来没有声明它 ⇒ 用户既看不到也关不掉（task-71 修的就是这个）。
 *
 * 这里**刻意不改 `types.ts`**（task-68 正在改 `ipc.ts`，避免两个人在同一处动类型；
 * 少一个声明不影响运行，因为 UI 用的就是后端快照）。代价是读写要绕一层类型，
 * 所以把这件事**只留在这两个符号里**，别在别处再抄一份。
 *
 * ⚠️ 保存能保住 `false`，靠的是 `patch` 里的 `{ ...settings, ...p }` **展开**
 * —— 未声明的属性会原样带过去，不会被 `serde(default = "yes")` 翻回 `true`。
 * 这条**有测试钉住**（`autoReconnectSetting.test.tsx` 的「关掉后读回仍是 false」），
 * 不是靠「恰好用了展开」的运气。
 */
type AutoReconnectPatch = Partial<AppSettings> & { auto_reconnect?: boolean };

/** 读开关：后端快照里带着它；万一缺失，按后端的默认值（`true`）显示。 */
function readAutoReconnect(s: AppSettings): boolean {
  return (s as { auto_reconnect?: boolean }).auto_reconnect ?? true;
}

export default function Settings({ focusSection }: { focusSection?: string | null } = {}) {
  const { snapshot, busy, run, runVoid } = useStore();
  const [draft, setDraft] = useState<AppSettings | null>(null);
  const [cat, setCat] = useState<SettingsCategoryId>(() => resolveInitialCategory(focusSection));
  const bodyRef = useRef<HTMLDivElement | null>(null);

  /**
   * 切换分类。
   *
   * ## 滚动位置：**切完回到内容顶部**
   *
   * 旧内容整个从 DOM 里消失，之前的滚动偏移在新分类里指向的是一段**无关**的中段；
   * 保持偏移会让人以为「点错了/内容没变」。回到顶部是唯一确定的位置。
   *
   * ## 记住上次分类：**记住**（会话级 sessionStorage）
   *
   * 设置页是「回来接着改」的地方：每次重新进入都回到第一类，会让连续调整（改 DNS →
   * 离开看日志 → 回来再改）每次都多点一次。记住的风险是「以为设置丢了」—— 这个风险
   * 很小，因为**记住的是用户自己上一次的选择**，而且分类标签上一直有明确的选中态。
   * 明确目标（跨页意图 / 深链）永远优先于记忆。
   */
  const selectCategory = useCallback((next: SettingsCategoryId) => {
    setCat(next);
    try {
      sessionStorage.setItem(LAST_CATEGORY_KEY, next);
    } catch {
      /* 隐私模式：记不住就算了，不影响功能 */
    }
    bodyRef.current?.scrollIntoView({ block: "start" });
  }, []);

  // 带目标的落地：**跨页意图**（`onNavigate("settings", "set-helper")`）与
  // **深链**（`#set-helper`）走同一套「目标 → 分类」解析。
  useEffect(() => {
    const target = focusSection ?? hashSection();
    if (!target) return;
    const targetCat = categoryOfSection(target);
    if (!targetCat) return;
    selectCategory(targetCat);
    // 落到分类还不够：目标分节要**被看见** —— 两级结构最容易把该看见的东西藏起来。
    // 用一次性类名（而不是 state）做强调：它不驱动重渲染，只是给用户「就是这里」的落点。
    const scrollTimer = window.setTimeout(() => {
      const el = document.getElementById(target);
      el?.scrollIntoView({ block: "center" });
      el?.classList.add("is-target");
    }, 0);
    const clearTimer = window.setTimeout(() => {
      document.getElementById(target)?.classList.remove("is-target");
    }, 2400);
    return () => {
      window.clearTimeout(scrollTimer);
      window.clearTimeout(clearTimer);
      document.getElementById(target)?.classList.remove("is-target");
    };
  }, [focusSection, selectCategory]);

  // 用户手改 hash（或从别处点 `#set-helper`）也走同一套解析。
  useEffect(() => {
    const onHash = () => {
      const id = hashSection();
      const c = id ? categoryOfSection(id) : null;
      if (c) selectCategory(c);
    };
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  }, [selectCategory]);

  if (!snapshot) return <div className="empty">正在加载…</div>;

  const active = SETTINGS_CATEGORIES.find((c) => c.id === cat) ?? SETTINGS_CATEGORIES[0];
  const activeIds = new Set<string>(active.sections.map((s) => s.id));
  const attention = categoryAttention(snapshot);

  const settings = draft ?? snapshot.settings;
  const dirty = draft !== null;

  const patch = (p: Partial<AppSettings>) => setDraft({ ...settings, ...p });
  /**
   * 写 `auto_reconnect`（前端类型里没声明的那个字段）。
   *
   * 先赋给一个「带该字段的可选子类型」再交给 `patch` —— 结构化类型下它是
   * `Partial<AppSettings>` 的子类型，所以这里**不需要 `as` 断言**。
   * 走的是与其它复选框**完全相同**的保存通路（`patch` → `draft` → `save`）。
   */
  const setAutoReconnect = (v: boolean) => {
    const nextPatch: AutoReconnectPatch = { auto_reconnect: v };
    patch(nextPatch);
  };
  const patchTun = (p: Partial<AppSettings["tun"]>) => patch({ tun: { ...settings.tun, ...p } });
  // 有进度就说明在下载：拿它当「正在下载」的判据，不用再开一个状态。
  const downloading = snapshot.update.progress !== null;

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
    <div className="set">
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

      {/* 分类导航 = 两级结构的第一级。
          为什么用 tablist/tab 而不是 `<a href="#set-…">` 锚点：
          ① 选中态要能被读出来（`aria-selected`），锚点读不出「当前是哪一类」；
          ② 键盘要能用 ←/→ 在分类间移动（WAI-ARIA tabs 的惯例），锚点只能逐个 Tab。
          深链仍然支持，但指向的是**分节 id**（`#set-helper`），由同一套解析落到所属分类。 */}
      <div className="set__tabs" role="tablist" aria-label="设置分类">
        {SETTINGS_CATEGORIES.map((c, i) => (
          <button
            key={c.id}
            type="button"
            role="tab"
            id={`set-tab-${c.id}`}
            aria-selected={cat === c.id}
            aria-controls={`set-panel-${c.id}`}
            // roving tabindex：Tab 进 tablist 停一次，之后用 ←/→ 换分类
            tabIndex={cat === c.id ? 0 : -1}
            className={`set__tab${cat === c.id ? " is-active" : ""}`}
            onClick={() => selectCategory(c.id)}
            onKeyDown={(e) => {
              const last = SETTINGS_CATEGORIES.length - 1;
              let next = i;
              if (e.key === "ArrowRight") next = i === last ? 0 : i + 1;
              else if (e.key === "ArrowLeft") next = i === 0 ? last : i - 1;
              else if (e.key === "Home") next = 0;
              else if (e.key === "End") next = last;
              else return;
              e.preventDefault();
              selectCategory(SETTINGS_CATEGORIES[next]!.id);
              document.getElementById(`set-tab-${SETTINGS_CATEGORIES[next]!.id}`)?.focus();
            }}
          >
            {c.label}
            {/* 注意力徽标：这一类里有**明确的**失败/未就绪。
                选「标签给指示」而不是「自动选中出问题的分类」—— 后者会在用户没要求时
                把内容换掉（静默跳转），徽标则是不打扰的常驻提示。
                用 role="img" + aria-label，不只用颜色表达。 */}
            {attention[c.id] && (
              <span
                className="set__tab-dot"
                role="img"
                aria-label="这一类里有需要处理的状态"
                title="这一类里有需要处理的状态"
              />
            )}
          </button>
        ))}
      </div>

      <div
        className="set__body"
        role="tabpanel"
        id={`set-panel-${cat}`}
        aria-labelledby={`set-tab-${cat}`}
        ref={bodyRef}
      >
      {/* ------------------------------------------------------- 代理入口 */}
      <Section id="set-entry" active={activeIds.has("set-entry")}>
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

        {/* 「自动连回来」——它以前**默认开着、却既看不到也关不掉**（task-71）。
            后端默认值是 `true`（`crates/xt-core/src/model.rs` 的 `#[serde(default = "yes")]`），
            所以「退出应用」并不能阻止下次启动把它拉起来：用户以为退出就没事了，
            而下次启动/登录项自启/自更新重启会**自动连回来**。
            放在「连接」这一类里、默认分类可见，正是因为它影响高、此前可见性为 0。 */}
        <label className="row" style={{ gap: 8, fontSize: 12, marginTop: 14 }}>
          <input
            type="checkbox"
            checked={readAutoReconnect(settings)}
            onChange={(e) => setAutoReconnect(e.target.checked)}
          />
          启动时如果上次是连接状态，自动连回来
        </label>
        {/* 文案逐句对应 `commands/core.rs` 的 `should_auto_reconnect`
            （四个条件缺一不可：`was_connected && auto_reconnect && mode != Direct && !already_running`）
            与 `reconnect_if_needed` 的注释（三种场景）。**不写「开机自动连接」** —— 那不是它的语义：
            它只在「上次确实连着」且非直连时把上次那条连接重建起来。 */}
        <div className="field__hint">
          只在三种情况下起作用：<strong>应用自更新</strong>（先退出、替换 App 后再启动）、
          <strong>崩溃后</strong>被系统重启、<strong>开机自启</strong>。
          前提是<strong>上次退出时确实是连着的</strong>，且当前不是「直连」模式 ——
          你主动点过「停止」的话，这里不会把隧道拉起来。
        </div>
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
      </Section>

      {/* ------------------------------------------------------- TUN */}
      {/* 用户的头号需求是「开机后自动连上，不需要点连接」，而开机自启动正是
          让这件事成立的开关 —— 所以它必须在上半屏。此前它在最底下的「其他」卡里，
          720px 窗口实测 top≈2997px（完全在折叠线下）。实测放在这里 top≈402px，抬头可见。 */}
      <Section id="set-autostart" active={activeIds.has("set-autostart")}>
        <h2 className="card__title">开机自启动</h2>
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

      </Section>

      <Section id="set-tun" active={activeIds.has("set-tun")}>
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
      </Section>

      {/* ------------------------------------------------------- DNS */}
      <Section id="set-dns" active={activeIds.has("set-dns")}>
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
      </Section>

      {/* ------------------------------------------------------- Fake-IP */}
      <Section id="set-fakeip" active={activeIds.has("set-fakeip")}>
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
      </Section>

      {/* ------------------------------------------------------- 内核 */}
      <Section id="set-core" active={activeIds.has("set-core")}>
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
      </Section>

      {/* ------------------------------------------------------- helper */}
      <Section id="set-helper" active={activeIds.has("set-helper")}>
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
          {/* 破坏性操作先确认（task-65）：用既有的 `InlineConfirm`，不引 window.confirm。
              问句里的后果是**读码确认**的（`crates/xt-proto/src/lib.rs` 的 `Restore` 定义 +
              `crates/xt-helper/src/server.rs` 的 `Request::Restore` 分支）：它会
              `tear_down_live_session()`（杀数据面、回滚路由与 DNS、关掉 utun fd）
              **再用磁盘快照 `force_cleanup()` 兜底** —— 也就是说它不只清「遗留会话」，
              **正在生效的隧道也会被拆掉**，helper 的回复是「已回滚路由与 DNS」。
              按钮文字里的「遗留配置」低估了这件事，所以确认问句必须说明白。 */}
          <InlineConfirm
            label="修复网络（回滚遗留配置）"
            className="btn"
            disabled={busy !== null}
            question="修复网络会回滚 helper 装的路由与 DNS，并拆掉当前正在生效的那条隧道（utun 网卡也会移除）——网络会回到直连；如果你正连着，连接会断。"
            confirmLabel="确认修复网络"
            onConfirm={() => void run("restore", () => api.restoreStale())}
          />
          {/* 卸载 helper 的后果（读码确认）：`Request::Uninstall` 的定义是
              「停止数据面、回滚会话、移除 launchd 注册与自身二进制」
              （`crates/xt-proto/src/lib.rs`）。之后没有 root 守护进程就**建不了 utun**，
              即 TUN 模式不可用；而重装要写 `/Library/LaunchDaemons` 与
              `/Library/PrivilegedHelperTools` → 本页自己的 hint 里写了「需要一次管理员授权」。 */}
          <InlineConfirm
            label="卸载 helper"
            className="btn btn--danger"
            disabled={busy !== null}
            question="卸载 helper 会停止数据面、回滚它装的路由与 DNS，并从 launchd 与磁盘上移除。之后 TUN 模式将不可用，要再使用需要重新安装并再次输入管理员密码。"
            confirmLabel="确认卸载"
            onConfirm={() => void run("uninstall-helper", () => api.uninstallHelper())}
          />
        </div>
        <div className="field__hint" style={{ marginTop: 10 }}>
          安装会写入 <span className="mono">/Library/LaunchDaemons</span> 与
          <span className="mono"> /Library/PrivilegedHelperTools</span>，需要一次管理员授权。
          发行版应改用 <span className="mono">SMAppService</span>（macOS 13+）：
          无需密码，但用户需要在「系统设置 → 通用 → 登录项与扩展 → 后台允许」里启用。
        </div>
      </Section>

      {/* ------------------------------------------------- DNS 解析器 */}
      <Section id="set-dns-probe" active={activeIds.has("set-dns-probe")}>
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
      </Section>

      {/* --------------------------------------------- 核心与 geo 更新 */}
      <Section id="set-update" active={activeIds.has("set-update")}>
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
          {/* 回退的后果（读码确认）：`revert_managed_update` 调
              `xt_core::update::revert_managed(managed_core_dir)`，它就是
              **删掉整个受管目录**（`crates/xt-core/src/update.rs:1273`：注释写明
              「清空托管目录 —— 回到包内自带的版本」），并记一条日志
              「已回退到随包版本（核心与 geo）」。
              ⚠️ **不写具体会变成哪个版本号**：随包核心的版本没有暴露在快照里，
              只有**当前受管的那个**版本可读（`core_managed_version`），所以问句只报
              「将要删掉的那个版本」，目标版本如实写「包内自带的那一版」。
              「需要重新连接才生效」也是可确认的：核心二进制在**启动时**才解析
              （`xray::resolve_core_binary(core_path, managed_core_dir, resource_dir, …)`）。 */}
          {snapshot.update.core_managed && (
            <InlineConfirm
              label="回退到随包版本"
              className="btn btn--ghost"
              disabled={busy !== null}
              question={`回退到随包版本会删掉受管更新里的核心${
                snapshot.update.core_managed_version
                  ? `（当前受管版本 ${snapshot.update.core_managed_version}）`
                  : ""
              }与 geo 文件，核心改回包内自带的那一版，需要重新连接才会生效。`}
              confirmLabel="确认回退"
              onConfirm={() => void run("revert-update", () => api.revertManagedUpdate())}
            />
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
            <button className="btn" disabled={busy !== null || downloading}
                    onClick={() => void run("check-app", () => api.checkAppUpdate())}>
              检查客户端更新
            </button>
            {/* 只在**确实**有新版时给「更新并重启」。
                `latest_app` 非空只说明「查到了 GitHub 上的最新版」—— 你装的就是它时
                也非空，只按它判断会让按钮永远显示（用户报的「多余」就是这个）。
                判据用后端算好的 `app_update_available`（它比过版本）。 */}
            {snapshot.update.app_update_available && snapshot.update.latest_app && (
              <button className="btn btn--primary" disabled={busy !== null || downloading}
                      onClick={() => void run("install-app", () => api.installAppUpdate())}>
                更新到 {snapshot.update.latest_app.version} 并重启
              </button>
            )}
            {/* 查到了、但确实没有新版 → 给一句明确的反馈。
                点了「检查客户端更新」总该有落点，否则用户会以为没生效（再去点第二次）。
                **失败时绝不允许走到这里**：`check_error` 优先（上面的失败块），
                因为「没查到」不等于「已是最新」。 */}
            {!snapshot.update.app_update_available &&
              snapshot.update.latest_app &&
              !snapshot.update.check_error && (
                <span className="field__hint" style={{ alignSelf: "center" }}>
                  已是最新版本
                </span>
              )}
            {downloading && (
              <span className="field__hint" style={{ alignSelf: "center" }}>下载中，请勿关闭…</span>
            )}
          </div>

          {snapshot.update.progress && <UpdateBar p={snapshot.update.progress} />}

          <div className="field__hint" style={{ marginTop: 8 }}>
            仓库是<span className="mono">公开</span>的，匿名就能查更新，所以不需要任何凭据。
            代价是匿名配额只有 <b>60 次/小时</b>且 GitHub <b>按 IP</b> 算 ——
            我们的请求大多经节点出去，等于和整台节点的用户共用这个额度，
            别人刷满时你这边会看到「限流」，过一会儿再试即可。
            <br />
            <b>安装会在替换 App 之后自动重启。</b>更新脚本先等你退出、再替换
            <span className="mono"> /Applications/XrayTun.app</span>，所以安装前请先
            断开隧道。校验用 release 里的 <span className="mono">SHA256SUMS.txt</span>
            （能防下载损坏，<b>防不了上游被换掉</b> —— 那需要签名，而这个包是 ad-hoc 签名），
            日志在 <span className="mono">~/Library/Logs/XrayTun/app-update.log</span>。
          </div>
        </div>
      </Section>

      {/* ------------------------------------------------------- 杂项 */}
      <Section id="set-misc" active={activeIds.has("set-misc")}>
        <h2 className="card__title">其他</h2>
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
        {/* ⚠️ 「退出时还原系统代理设置」这个复选框**已从界面移除**（task-65）。
            原因：它绑定的 `settings.restore_system_proxy_on_exit` 在**全仓没有任何逻辑读它**
            —— 只有 `crates/xt-core/src/model.rs` 的字段定义与默认值、`types.ts` 的类型、
            预览数据，以及这里原来的复选框本身。也就是说**用户勾与不勾，什么都不会发生**。
            界面**不许承诺做不到的事**（本项目红线），比「少一个功能」严重。

            它当前**没有对象**：`ProxyMode::SystemProxy` 只提供本机 SOCKS/HTTP 入站，
            **从来没有改过 macOS 的系统代理设置**（`docs/07-roadmap-and-risks.md` 的
            「已知未实现项」第 6 条），所以「退出时还原」没有被还原的东西。

            实现它（写用户的网络服务设置 + 崩溃安全还原）是**独立的路线图功能项**，
            不该由一张「加确认框」的卡顺手带出来。
            字段与 `serde` 默认值**刻意保留**（删掉会破坏已持久化设置文件的兼容性），
            只隐藏界面入口；将来真正实现后，再把开关放回来，并同时补一句如实的说明。 */}

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
      </Section>
      </div>
    </div>
  );
}

/** 更新下载进度条。
 *
 *  `total_bytes` 为 null 时上游没报字节数 —— 这时**不画百分比**，只显示
 *  已下载多少。画一个假的百分比比不画更糟。
 */
function UpdateBar({ p }: { p: UpdateProgress }) {
  const pct =
    p.total_bytes && p.total_bytes > 0
      ? Math.min(100, Math.round((p.done_bytes / p.total_bytes) * 100))
      : null;
  return (
    <div style={{ marginTop: 10 }}>
      <div className="row" style={{ justifyContent: "space-between", fontSize: 11 }}>
        <span>正在下载 {p.label}</span>
        <span className="mono">
          {fmtBytes(p.done_bytes)}
          {p.total_bytes ? ` / ${fmtBytes(p.total_bytes)}` : ""}
          {pct !== null ? ` · ${pct}%` : ""}
        </span>
      </div>
      <div
        style={{
          marginTop: 4,
          height: 6,
          borderRadius: 3,
          background: "var(--border)",
          overflow: "hidden",
        }}
      >
        <div
          style={{
            height: "100%",
            // 不知道总量时用一个固定的小宽度表示「在动」，而不是假装有进度。
            width: pct !== null ? `${pct}%` : "30%",
            background: "var(--accent, var(--ok))",
            transition: "width 200ms linear",
          }}
        />
      </div>
    </div>
  );
}

function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(0)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}
