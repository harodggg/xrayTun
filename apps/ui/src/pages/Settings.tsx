import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { api } from "../ipc";
import { CopyButton } from "../IncidentReport";
import { InlineConfirm } from "../InlineConfirm";
import SnapshotFallback from "../SnapshotState";
import { useStore } from "../store";
import {
  DNS_MODE_LABEL,
  IPV6_LABEL,
  MODE_LABEL,
  UpdateProgress,
  type AppSettings,
  type AppSnapshot,
  type DnsHandling,
  type HelperVersionCheck,
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

/**
 * Xray 真正认得的 `loglevel` 取值。
 *
 * 依据：Xray 的 `infra/conf/log.go` 只对 debug/info/warning/error/none 做映射，
 * 其余取值走 `default` → **warning**；仓内 `config.rs:177` 是原样透传，
 * `model.rs:768` 的 `log_level` 就是 `String`，所以前端是唯一的把关点。
 * 上游文档：https://xtls.github.io/en/config/log.html
 */
const XRAY_LOG_LEVELS = ["none", "error", "warning", "info", "debug"];

/**
 * 「遗留会话」这一行该说什么（task-152 / A13）。
 *
 * # 为什么不能只看 `stale_session`
 *
 * 这个字段的判据是 `SessionSnapshot::is_stale()`＝「**崩在半路**」。而一条
 * **提交完路由**的会话状态是 `Up`，不算崩在半路 ⇒ 它**不会**出现在 `stale_session` 里，
 * 但 helper 内存里确实挂着它（`helper.tun_active = state.session.is_some()`，
 * `crates/xt-helper/src/server.rs:272-276`）。
 *
 * # 但也不能只看 `tun_active`（这一步很关键）
 *
 * `tun_active` 是「helper 里有一条**活的**会话」，**正常连接时它也是 true**！
 * 所以 `tun_active && !stale_session` **不等于**「遗留」—— 直接在界面上报警会在
 * **每一次正常连接**时误报（本项目对这种假警报零容忍）。
 *
 * 区分依据是 `runtime.running`：
 * * **核心在跑** ⇒ 这条会话就是本 App 的当前隧道（Dashboard 的「 · 有活跃隧道」
 *   说的正是它，两处不再互相打脸）；
 * * **核心没在跑** ⇒ 没人接管它 —— 这正是 `apps/desktop/src/lib.rs:242` 的启动判据
 *   `orphaned = stale_session.is_some() || tun_active` 想抓的东西；那里的前提成立，
 *   是因为它在 **setup 阶段**运行、**本 App 的核心还没起来**（`:301` 才开始连）。
 *   界面在整个运行期都要渲染，所以必须多这一个条件。
 *
 * 措辞口径：**不夸大、不承诺** —— 只说「有一条没人接管的会话」与「可以点修复网络」，
 * 不写「已自动清理」之类（启动时的回滚失败会留下这一态，`:255` 的日志里有
 * 「自动修复失败，请点「修复网络」重试」）。
 */
export function legacySessionView(
  staleSession: string | null,
  tunActive: boolean,
  coreRunning: boolean,
): string {
  if (staleSession) return `${staleSession}（磁盘快照：上次崩在半路的会话）`;
  if (tunActive && !coreRunning) {
    return "有：helper 里挂着一条活的 TUN 会话，但本 App 没有在跑核心 —— 没人接管它，" +
      "也没有可回滚的磁盘快照。点「修复网络」可以拆掉它（路由与 DNS 会一起还原）。";
  }
  if (tunActive) return "无（当前的隧道会话由本次运行管理）";
  return "无";
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

  // task-23 A1：读不到快照时不再写「正在加载…」（永远不停的等待），
  // 而是分「正在读取」与「读失败 + 重试」两态。
  if (!snapshot) return <SnapshotFallback />;

  const active = SETTINGS_CATEGORIES.find((c) => c.id === cat) ?? SETTINGS_CATEGORIES[0];
  const activeIds = new Set<string>(active.sections.map((s) => s.id));
  const attention = categoryAttention(snapshot);

  const settings = draft ?? snapshot.settings;
  const dirty = draft !== null;

  const patch = (p: Partial<AppSettings>) => setDraft({ ...settings, ...p });
  const patchTun = (p: Partial<AppSettings["tun"]>) => patch({ tun: { ...settings.tun, ...p } });
  // 有进度就说明在下载：拿它当「正在下载」的判据，不用再开一个状态。
  const downloading = snapshot.update.progress !== null;

  /**
   * 助手版本对照（后端三态：`match` / `mismatch` / `unreadable`）。
   *
   * 类型上后端**一定**会带它（`build_snapshot` 里无条件赋值），但**预览快照还没有这个字段**
   * （`previewSnapshot.ts` 的 `helper` 块是 `as unknown as` 转的，属已知的保真度缺口）。
   * 所以这里按「可能缺失」处理：缺字段时**什么都不显示** ——
   * 缺字段既不等于「一致」也不等于「不一致」，猜任何一个都是编造。
   */
  const versionCheck: HelperVersionCheck | undefined = snapshot.helper.version_check;

  /**
   * IPv6 下拉里**真正提供**的两个选项（task-142）。
   * 第三个值 `disabled` 仍然存在于 `xt-proto` 的枚举里（旧请求兼容），但界面不再给 ——
   * 它今天与 `passthrough` 逐字节相同，摆出来就是一句假承诺。
   */
  const IPV6_CHOICES: Ipv6Mode[] = ["passthrough", "override"];
  /** 已存的值 `disabled` 按「不接管」显示（最简 UI 侧兼容；不做静默迁移）。 */
  const ipv6Shown: Ipv6Mode = settings.tun.ipv6 === "disabled" ? "passthrough" : settings.tun.ipv6;

  /**
   * A12 的两个派生值。**都来自真实字段**：
   * * `fakednsOn` —— `settings.fakedns.enabled`；
   * * `sniffingEffective` —— 后端生成配置时的真值 `sniffing || fakedns`
   *   （`crates/xt-core/src/xray/config.rs:346`），所以 Fake-IP 开着时它恒为 true。
   */
  const fakednsOn = settings.fakedns.enabled;
  const sniffingEffective = settings.dns.sniffing || fakednsOn;

  const patchDns = (p: Partial<AppSettings["dns"]>) => patch({ dns: { ...settings.dns, ...p } });
  const patchFake = (p: Partial<AppSettings["fakedns"]>) =>
    patch({ fakedns: { ...settings.fakedns, ...p } });

  /** 保存草稿。返回**是否真的存下去了** —— 调用方（`restart`）必须看这个返回值。 */
  const save = async (): Promise<boolean> => {
    if (!dirty) return true;
    const ok = await run("save", () => api.saveSettings(settings));
    if (ok) setDraft(null);
    return ok;
  };

  /**
   * 「保存并重启核心」。
   *
   * task-120：这里原来写的是 `if (dirty) await save();` —— **不看返回值就重启**。
   * 保存失败（例如端口填 80 被 `model.rs:854-859` 拒绝）时：
   * 1. 界面不会停在这里，而是继续往下重启；
   * 2. `run("restart", …)` 在 `store.tsx` 里先 `setError(null)`，把刚写上的
   *    那条保存错误**抹掉**；核心于是用**旧设置**重启。
   * 用户看到的是一次「成功」的重启，以为改动已经生效 —— 而它根本没进配置。
   * 现在保存失败就停下，错误留在界面上。
   */
  const restart = async () => {
    if (!(await save())) return;
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
            checked={settings.auto_reconnect}
            onChange={(e) => patch({ auto_reconnect: e.target.checked })}
          />
          启动时如果上次是连接状态，自动连回来
        </label>
        {/*
          task-138：这一段原来写「**只在三种情况下起作用**：应用自更新 / 崩溃后被系统重启 /
          开机自启」。逐行核过 Rust 后（见下），它漏了一种**真实且常见**的情形，
          并且把开关的适用范围说宽了：

          * `reconnect_if_needed` **只有一个调用点**：`apps/desktop/src/lib.rs:301`
            （setup 钩子里 spawn；重试 `RECONNECT_ATTEMPTS` 次）。判据
            `should_auto_reconnect`（`commands/core.rs:503-516`）=
            `was_connected && auto_reconnect && mode != Direct && !running`。
            **开关只在这一条路径上生效。**
          * 正常退出**不动作废**那个意图：`RunEvent::ExitRequested` → `tray::sync_cleanup`
            （`lib.rs:126-128` / `tray.rs:165-205`）只做「回滚网络 + 杀核心进程 + 内存里
            标记 running=false」，**从不写 `was_connected`**；它只在
            `stop_proxy`（`core.rs:90`，UserStop）与已知失败退场（`core.rs:665`）被清零
            （`invalidate_connect_intent`，`core.rs:577-584`）。这一点代码自己的注释也写着：
            「『退出应用』因此只在本次进程内有效」（`core.rs:553-556`）。
            ⇒ **「你自己退出 App 后再次打开」是第四种情形**，旧文案没列。
          * 运行期间的自愈**不看这个开关**：看门狗重建（`core.rs:1330`，判据
            `should_rebuild_tunnel`，`core.rs:716`）与换网/出口变化重建（`core.rs:1725`，
            判据 `should_rebuild_after_egress_change`，`core.rs:1553`）都只读
            `was_connected`（`core.rs:1263` / `:1656`），没有 `auto_reconnect` 这一项。
            ⇒ 「关掉开关」≠「不会再自动连」，用户必须知道这一点（否则会以为关了它就绝对安全）。
          所以下面按**开关的真实值**分两句写，不是一段静态说明。
        */}
        {settings.auto_reconnect ? (
          <div className="field__hint">
            它只管<strong>启动时那一次</strong>：上次退出时是连着的、且当前不是「直连」模式，
            才会把上次那条连接拉回来。会走到它的有<strong>四种情形</strong>：
            <strong>开机自启</strong>、<strong>应用自更新</strong>后重启、
            <strong>崩溃后</strong>被系统重启，以及<strong>你自己退出 App 后再次打开</strong>
            （正常退出不会作废这个意图 —— 只有你点过「断开」、或引擎判定为"已知失败"才会）。
            <br />
            ⚠︎ 它管不住<strong>运行期间的自愈</strong>：隧道已经连着时，
            <strong>看门狗重建</strong>（连续多次不通）与<strong>换网重建</strong>只看
            「你是否还想连着」，<strong>不看这个开关</strong>。
          </div>
        ) : (
          <div className="field__hint">
            现在<strong>关着</strong>：<strong>启动时</strong>不会自动连回 ——
            上面那四种情形（开机自启 / 应用自更新后重启 / 崩溃后被系统重启 /
            你自己退出后再次打开）都不会把隧道拉起来。
            <br />
            ⚠︎ 但「关掉它」<strong>不等于</strong>「不会再自动连」：隧道已经连着时，
            运行期间的<strong>看门狗重建</strong>与<strong>换网重建</strong>
            <strong>不看这个开关</strong>，只看「你是否还想连着」这个意图。
            要真的让它停下来，请点顶栏的「<strong>断开</strong>」——
            那会同时作废「下次自动连回」的意图。
          </div>
        )}
        <div className="field" style={{ marginTop: 14 }}>
          <label>日志级别</label>
          {/*
            task-120：这里原来是 silent / error / warning / info / debug。
            `config.rs:177` 把 `settings.log_level` **原样**写进 Xray 的 `"loglevel"`
            （Rust 侧只是 `String`，无校验），而 Xray 只认
            debug / info / warning / error / **none** —— `silent` 落到它的
            `default` 分支，实际按 **warning** 处理。也就是选「silent」等于什么都没关掉，
            而真正静音的 `none` 界面上根本没有。
            现在给出 Xray 真正认得的五个值；如果配置里存着历史值（例如 `silent`），
            就把它作为一项如实标出来，而不是让下拉框显示成别的值。
          */}
          <select value={settings.log_level} onChange={(e) => patch({ log_level: e.target.value })}>
            {!XRAY_LOG_LEVELS.includes(settings.log_level) && (
              <option value={settings.log_level}>
                {settings.log_level}（当前保存值；Xray 不识别，实际按 warning 处理）
              </option>
            )}
            <option value="none">none（不输出日志）</option>
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

        {/* ── 高级参数收进折叠（task-78）────────────────────────────────
            为什么：这些旋钮**默认值几乎总是对的、改了才需要懂**，而「连接」是用户最常打开的分类
            —— 摊开时实测要 1158px（视口 813px），用户为了改「代理入口」得先滚过一屏多。

            复用**本项目已有的折叠范式**：`Routing.tsx` 的 `<details className="page__details">`
            （原生 details/summary + 现有样式类，含 ▸ 指示与悬停态）。
            **没有新造机制、没有新 CSS**；原生 details 不卸载子节点，所以受控输入的值得以保留。

            ⚠️ **留在明面的三项是刻意选的**：
            · **哨兵 DNS**：它的说明是「隧道没了而 DNS 还指着它 ⇒ 用户表现为全网断」这条风险的
              **唯一解释**，默认看不见就把已知风险变成暗知识；
            · `bypass_private`：用户可感知、决定「家里 NAS / 局域网设备还能不能直连」，
              且是网络出问题时最先要确认的东西 —— 这类「自救」项不许进折叠；
            · 本节标题与「当前模式」说明：它是这一节的上下文，不该被藏。
            被收起的**说明文字一句没删**；summary 里把四项名字都列出来，
            收起时用户仍然知道里面有什么，点开即见完整解释。 */}
        <details className="page__details">
          <summary>高级：隧道网段 / MTU / IPv6 / 出站绑定接口</summary>

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
            {/*
              task-142（A11，Lead 裁决 2b）：**这里原来有三个选项，其中「禁用 IPv6」是一句
              假承诺**。依据（只读核对）：

              * `Ipv6Mode::Disabled` 与 `Passthrough` 在路由计划里落在**同一个空分支**
                （`crates/xt-tun/src/plan.rs:233-243`），而 `Ipv6Mode::Disabled` 在**全仓
                只出现一次**就是那处（`grep -rn "Ipv6Mode::Disabled" crates apps`）。
              * 核心侧也只认 `Override`（`crates/xt-core/src/xray/config.rs:104-112`），
                没有任何地方会因为 `disabled` 去阻断 v6。
              ⇒ 选它**什么都不会发生**，但用户会得到「我已经防住 v6 泄漏」的错误信念。

              **没有动的**：`crates/xt-proto` 的枚举（旧请求里可能带着 `disabled`，
              删枚举会破坏兼容）、Rust 语义、以及已存值本身 —— UI 只是不再**提供**它，
              并把已存值按「不接管」显示（下面 `ipv6Shown`）。见 `docs/design/DECISION-A11-A12.md`。
            */}
            <div className="field">
              <label>IPv6 处理</label>
              <select
                value={ipv6Shown}
                onChange={(e) => patchTun({ ipv6: e.target.value as Ipv6Mode })}
              >
                {IPV6_CHOICES.map((k) => (
                  <option key={k} value={k}>
                    {IPV6_LABEL[k]}
                  </option>
                ))}
              </select>
              <div className="field__hint">
                选「不接管」时，IPv6 流量会绕过隧道走物理网卡 —— 可能泄漏真实出口，
                但不会因为内核 IPv6 配置问题导致断网。这是一个刻意的取舍。
                <br />
                本版本<strong>不提供「禁用 v6」这一档</strong>：它在路由计划里与「不接管」
                逐字节相同（没有任何代码会因为它去阻断 v6），摆出来就是一句假承诺。
                所以当前版本 TUN <strong>不会阻断 IPv6</strong>，v6 流量仍然走物理网卡。
                要真正避免 v6 泄漏，请在<strong>系统层面</strong>关闭 IPv6。
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
        </details>

        {/* ⚠️ 哨兵 DNS **刻意留在明面**（不随上面几项一起收进「高级」）：它的说明是
            「隧道没了而 DNS 还指着它 ⇒ 用户表现为全网断」这条风险的**唯一解释**。
            这类解释一旦默认看不见，风险就从「已知」变成「暗知识」。 */}
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
        {/*
          task-142（A12，Lead 裁决选项 1）：**这个复选框在 Fake-IP 打开时不生效。**
          依据（只读核对）：生成配置时嗅探的开关是
          `crates/xt-core/src/xray/config.rs:346` 的 `s.dns.sniffing || s.fakedns.enabled`
          —— 开了 Fake-IP，嗅探恒为真（`destOverride` 也随之为 `fakedns+others`，
          `routeOnly` 必须保持 false，否则假地址还原不回来）。
          所以这里做三件事：① 显示**实际生效**的值；② Fake-IP 开着时让它不可点（点了也没用）；
          ③ 把原因与**真正能关掉嗅探的动作**写出来。—— 行为一行未改（后端仍是那个 `||`）。
        */}
        <label className="row" style={{ gap: 8, fontSize: 12 }}>
          <input
            type="checkbox"
            checked={sniffingEffective}
            disabled={fakednsOn}
            onChange={(e) => patchDns({ sniffing: e.target.checked })}
          />
          开启流量嗅探（按 SNI / Host 分流）
        </label>
        {fakednsOn ? (
          <div className="field__hint" style={{ marginTop: 6 }}>
            现在显示的是<strong>实际生效</strong>的值：<strong>Fake-IP 已开启，嗅探被强制打开</strong>
            （生成配置时是 <span className="mono">sniffing || fakedns</span>），
            因为假地址要靠嗅探还原成域名。所以这个复选框现在<strong>不可点，关掉它不生效</strong>。
            <br />
            真正能关掉嗅探的动作：先到<strong>「连接」分类里关闭 Fake-IP</strong>，
            再回来关这个复选框。另外，关掉嗅探之后域名分流只能依赖 DNS 阶段的信息，
            对「直接用 IP 发起连接」的程序会失效。
          </div>
        ) : (
          <div className="field__hint" style={{ marginTop: 6 }}>
            关闭嗅探后，域名分流只能依赖 DNS 阶段的信息，对「直接用 IP 发起连接」的程序会失效。
            （Fake-IP 没开，所以这里关掉就是真的关掉了。）
          </div>
        )}
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
        {/* task-142（A12 反向说明）：两个开关在**两个分类**里，用户会来回猜。
            这一句把耦合写在 Fake-IP 这一侧（依据同样是 `config.rs:346` 的 `sniffing || fakedns`）。 */}
        <div className="field__hint" style={{ marginTop: 6 }}>
          开启 Fake-IP <strong>会同时把「流量嗅探」强制打开</strong>（假地址要靠嗅探还原成域名）——
          那之后到「DNS 解析器」里取消勾选嗅探<strong>不生效</strong>；想关掉嗅探，先关掉这里的 Fake-IP。
        </div>
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
          <dd>{legacySessionView(snapshot.helper.stale_session, snapshot.helper.tun_active, snapshot.runtime.running)}</dd>
        </dl>

        {/* ── 助手版本核对（task-84 的 Rust 半 + task-86 的 UI 半）────────────
            **App 更新不会刷新特权助手**：`restart_helper` 只 kickstart 磁盘上那份旧二进制，
            只有 `install_helper` 才把包内那份拷过去（`commands/helper.rs` 的注释）；
            而**路由/DNS 的安装与回滚都在 helper 里** ⇒ 两者不一致时，用户以为「更新拿到了
            全部修复」，**helper 侧那部分其实没生效**，而且这件事完全无声。

            后端给**三态**，界面**不许把它们合并**：
            · `mismatch`   → 说出来 + 一键重装（**复用既有 install 通路**）
            · `match`      → **什么都不显示**（否则就是狼来了）
            · `unreadable` → **如实说读不到**，不猜成一致、也不猜成不一致
            ⚠️ **绝不静默自动重装**：那是特权操作（要管理员授权），必须由用户点。
            ⚠️ 字段缺失时（预览快照还没带上它 —— 已知的保真度缺口，Lead 已记入清理清单）
            **什么都不显示**：缺字段既不等于 match、也不等于 mismatch，不许猜。 */}
        {versionCheck?.state === "mismatch" && (
          <div className="banner banner--warn" role="status" style={{ marginBottom: 14 }}>
            <span>⚠︎</span>
            <div>
              {/* task-154 B5：原来写「已安装的助手是 X，随 App 附带的是 Y —— **两者不一致**」，
                  而判据是**协议号相等**（`commands/helper.rs`），`mismatch` 里
                  **包含「两边包版本相同、协议号不同」**这一情形 ⇒ 那句话会出现
                  「0.8.33 与 0.8.33 两者不一致」的自相矛盾，用户会怀疑产品在胡说、
                  或者白重装一次特权助手。现在只说**判据**与**该做什么**，
                  不对两个版本号的关系下结论（协议号本身没有暴露在快照里，不编）。 */}
              助手与 App 的<strong>兼容性检查没通过</strong>（判据是<strong>协议号</strong>，
              不是包版本）。已安装的助手是 <span className="mono">{versionCheck.installed}</span>，
              随 App 附带的是 <span className="mono">{versionCheck.bundled}</span>
              —— <strong>版本号相同也可能不兼容</strong>（两边协议号不同就会走到这里）。
              助手负责安装路由与 DNS，<strong>不重装的话，助手侧的这部分修复不会生效</strong>
              （App 本身已经更新，其余修复不受影响）。
              <div style={{ marginTop: 8 }}>
                <button
                  className="btn btn--primary"
                  disabled={busy !== null}
                  onClick={() => void run("install-helper", () => api.installHelper())}
                >
                  {busy === "install-helper" ? <span className="spin" /> : null}
                  重新安装助手
                </button>
              </div>
            </div>
          </div>
        )}
        {versionCheck?.state === "unreadable" && (
          <div className="field__hint" style={{ marginBottom: 12 }}>
            无法核对助手版本：{versionCheck.reason}
            —— 所以这里既不说「一致」，也不说「不一致」。
          </div>
        )}

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
          {/*
            task-128（B7）：上面那段注释写的是 `Request::Uninstall` 会「回滚会话」，
            但**实际走的是 shell 脚本**（`commands/helper.rs:39` 的 `uninstall_script()`
            → `helper_install.rs:142-161`），它只做 `launchctl bootout` + 删 plist/二进制/socket。
            路由与 DNS 的回滚是 `bootout` 发出 SIGTERM 后 **helper 自己的信号处理器**
            干的（`crates/xt-helper/src/main.rs:168-170` → `graceful_shutdown()` → `:197`）。
            ⇒ **没有活着的 helper 进程时，那一步不会发生**：路由/DNS 会留在系统里。
            而 helper「已安装但没在跑」正是本页明确支持的状态（`:834` 专门给它一颗重启按钮）。
            判据用 `helper.reachable`（有没有进程在应答）—— 它是这一串因果关系的前置条件，
            不是猜的。
          */}
          <InlineConfirm
            label="卸载 helper"
            className="btn btn--danger"
            disabled={busy !== null}
            question={
              snapshot.helper.reachable
                ? "卸载 helper 会停止数据面、回滚它装的路由与 DNS，并从 launchd 与磁盘上移除。之后 TUN 模式将不可用，要再使用需要重新安装并再次输入管理员密码。"
                : "卸载 helper 会从 launchd 与磁盘上移除它，并停止数据面。<strong>但助手当前没有在应答（没在跑）</strong>，而回滚路由与 DNS 是它收到停止信号后自己做的 —— 没有进程时这一步不会发生，<strong>系统里可能残留它装过的路由与 DNS</strong>。建议先用左边的「修复网络」清一遍，再卸载。之后 TUN 模式将不可用，要再使用需要重新安装并再次输入管理员密码。"
            }
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
          /** 自动选优关着时，探测结果不会写回配置 ⇒ 「首选」必须说清是谁。 */
          const autoSelect = snapshot.settings.dns.auto_select;
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

              {/*
                task-120：`dns.chosen` 是**本次探测**算出的首选，它只有在
                `settings.dns.auto_select` 为真时才会被写回 `direct_servers`
                （`commands/snapshot.rs:136-160` 的 `if settings.dns.auto_select` 分支）。
                自动选优关着的时候，旧文案「当前首选 X」是假的 —— 配置里生效的是
                `direct_servers[0]`，与 X 无关。所以分成两句，并且把真正生效的那个写出来。
              */}
              {chosen &&
                (autoSelect ? (
                  <div className="field__hint" style={{ margin: "6px 0 0" }}>
                    本次检测首选 <span className="mono">{chosen}</span>
                    （自动选优已开启，会写回配置）
                  </div>
                ) : (
                  <div className="field__hint" style={{ margin: "6px 0 0" }}>
                    本次检测最快 <span className="mono">{chosen}</span> —— 自动选优已关闭，
                    配置里生效的是{" "}
                    <span className="mono">
                      {(isCn
                        ? snapshot.settings.dns.direct_servers
                        : snapshot.settings.dns.remote_servers
                      )[0] ?? "（空）"}
                    </span>
                  </div>
                ))}

              <div className="probe-table">
                {rows.map((p) => (
                  <div key={p.server} className="probe-table__row">
                    <span className="mono">{p.server}</span>
                    <span className="field__hint">{p.label}</span>
                    {/*
                      task-120：原来 `latency_ms === null` 就一律写「不通」，而
                      `answered` 是**另一个**字段：`dns_probe.rs:445-447` 里
                      `answered = 参照域名答出来了 || 有延迟采样`，所以完全存在
                      「**答得出、但 3 次延迟采样都超时** ⇒ `answered: true, latency_ms: null`」，
                      这时旧文案说「不通」，而同一行的颜色（按 `!p.answered` 算）却是绿色 ——
                      一句话和它自己的颜色互相打脸。
                    */}
                    <span className={`badge badge--${p.suspect || !p.answered ? "slow" : "fast"}`}>
                      {p.latency_ms !== null
                        ? `${p.latency_ms} ms`
                        : p.note
                          ? "未探测"
                          : p.answered
                            ? "答得出，量不到延迟"
                            : "不通"}
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
          <span className="mono">国外</span> 只有经节点才连得上，所以经本地 SOCKS
          入站去测 —— 那也正是它在分流规则里被使用时的路径。
          {/*
            task-128（B17）：原来的写法是「直连 1.1.1.1:443 **实测 8 秒超时**…
            **没连接节点时**这一组显示「未探测」」。两处都不对：
            * 「8 秒」只出现在 `snapshot.rs:79` 的注释与 `docs/04-routing-and-dns.md`，
              代码里的探测超时是 **2 秒**（`snapshot.rs:93`）—— 我核不到 8 秒这个数字，
              所以不写它；
            * 「未探测」的判据是 `spec.socks.is_none()`，而 `socks` 来自
              **核心在跑**（`snapshot.rs:85-90`），不是「选了节点」：核心在跑但没选节点时
              这一组照样被探测，必然超时被记成「不通」。
            所以这里改成一个**可真可假**的判断，判据是 `runtime.running`。
          */}
          {snapshot.runtime.running ? (
            <>
              {" "}
              现在<strong>核心在跑</strong>，所以这一组是<strong>真的在探测</strong>（经上面的本地 SOCKS
              入站）；测不出来会如实写成「不通」或「答得出，量不到延迟」。
            </>
          ) : (
            <>
              {" "}
              现在<strong>核心没在跑</strong>（没有本地 SOCKS 入站在监听），所以这一组
              <strong>没有探测</strong> —— 显示「未探测」，而不是拿直连的超时冒充「都不通」。
            </>
          )}
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
              {/* task-154 B9：原来 `?? "未找到"`。而 `core_version` 来自
                  `core.version`（`snapshot.rs:587`），= `xray version` 的第一行；
                  核心**找得到但这条命令没给出可解析输出**时它也是 null ⇒
                  界面上「内核」一节显示着路径、这里却说「未找到」，用户会以为核心没装。
                  现在按 `core.path` 区分两种 null（不编版本号）。 */}
              {snapshot.update.core_version ??
                (snapshot.core.path
                  ? "读不到版本（核心在，但 `xray version` 没有给出可解析的输出）"
                  : "未找到核心")}
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

        {/* task-194 + task-195：这一节涵盖**核心/geo/客户端**三类，所以这里继续读
            **派生**的合并字段 `check_error`（一次失败必须说出来，这正是它该在的位置）。
            但**必须标明是哪一类** —— 只说「检查更新失败」会被读成「客户端检查失败」而其实可能是核心。

            task-195 之后合并字段是**按 客户端 → 核心 → geo 取第一条非空**派生出来的，
            而且三个子系统各有自己的格子（`check_error_app` / `check_error_core` / `check_error_geo`）
            ⇒ 这里可以直接读**具体哪一格**来标注，不再靠「非 app 即核心」这种二选一猜测
            （那会把 **geo** 的失败误标成核心）。 */}
        {snapshot.update.check_error && (
          <div className="banner banner--warn" style={{ marginTop: 10 }}>
            <span>⚠︎</span>
            <div>
              {snapshot.update.check_error_app
                ? `客户端检查更新失败：${snapshot.update.check_error_app}`
                : snapshot.update.check_error_geo
                  ? `geo 数据检查更新失败：${snapshot.update.check_error_geo}`
                  : `核心检查更新失败：${
                      snapshot.update.check_error_core ?? snapshot.update.check_error
                    }`}
            </div>
          </div>
        )}

        <div className="field__hint" style={{ marginTop: 10 }}>
          更新装在数据目录里，<strong>不会改动 App 包本身</strong>（改包内文件会让签名失效），
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
              {/*
                task-128（B8）建了这条标签、task-194 把判据改绑到**客户端专属**字段。

                口径没变：客户端在**检查失败**时后端**不清** `latest_app`
                （`version_check.rs::apply_app_check_result` 的 `Err` 分支只写错误、`Ok` 才写
                `latest_app`），而 `app_update_available` 每个快照都按残留的 `latest_app` 重算 ⇒
                「客户端检查失败」与「有一个可装的版本」会同时成立，标签必须如实说这是**上次**查到的。

                **改绑的理由**：原来读合并的 `check_error`，于是**核心/geo** 失败时也会说
                「本次没查成」—— 可那次客户端检查其实是成功的（`task-194`：陈述不实）。
                客户端专属判据与仪表盘 chip（`task-193`）同一套：`check_error_app`。
              */}
              <div className="kv__k">
                {snapshot.update.check_error_app && snapshot.update.latest_app
                  ? "上次查到的最新版（本次没查成）"
                  : "GitHub 上的最新版"}
              </div>
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
            {/*
              上面那条判断的补充（task-128 B8 建、task-194 改绑）：**客户端**这一次检查失败时
              **不给**安装按钮 —— 否则就是拿一次失败检查的残留值劝用户升级。版本号本身照旧显示
              在右上，只是标明它是上次查到的。

              ⚠️ **task-194 的功能性修复**：这里原来用合并的 `check_error` ⇒ **核心/geo 检查失败**
              会让这颗按钮**直接消失**，而客户端的结论（`app_update_available` / `latest_app`）其实
              是**刚刚成功**那次查到的 —— 「已知有新版却无从安装」。改用 `check_error_app` 后，
              只有**客户端**那条线失败才收按钮。
            */}
            {snapshot.update.app_update_available &&
              snapshot.update.latest_app &&
              !snapshot.update.check_error_app && (
              <button className="btn btn--primary" disabled={busy !== null || downloading}
                      onClick={() => void run("install-app", () => api.installAppUpdate())}>
                更新到 {snapshot.update.latest_app.version} 并重启
              </button>
            )}
            {/* 查到了、但确实没有新版 → 给一句明确的反馈。
                点了「检查客户端更新」总该有落点，否则用户会以为没生效（再去点第二次）。
                **失败时绝不允许走到这里**：客户端失败时优先说失败（上面的失败块），
                因为「没查到」不等于「已是最新」。
                ⚠️ **task-194**：判据同样只能用客户端专属字段 —— 用合并的 `check_error` 会让
                核心/geo 的失败**顺手把这条回执也吞掉**（用户点了检查却像没生效）。 */}
            {!snapshot.update.app_update_available &&
              snapshot.update.latest_app &&
              !snapshot.update.check_error_app && (
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
          在顶栏与菜单栏显示实时网速
        </label>
        {/*
          task-140：这段原来写「**窗口标题栏**显示 ↓ 1.2 MB/s ↑ 34 KB/s」。两处都不对，
          而且第 1 处会让用户按图索骥找不到东西（他会以为功能坏了）：

          * **原生标题栏上的文字是看不见的**：`apps/desktop/tauri.conf.json` 的窗口配置写着
            `"titleBarStyle": "Overlay"` + `"hiddenTitle": true`（macOS 会隐藏原生标题文字）。
            界面上那条带速率的「标题栏」是 **webview 自己画的顶栏**
            （`apps/ui/src/App.tsx` 的 `TopBar`，`:262-275`）。Rust 侧同一件事也写在
            `apps/desktop/src/traffic.rs:113-124` 的注释里，并且说明
            `window.set_title` 仍然保留 —— 它决定的是**「窗口」菜单与 Mission Control**
            里显示什么（我未在真机验证这两处的渲染，见诚实清单）。
          * **单位不对**：顶栏用的是 `types.ts` 的 `formatRate` → `formatBytes`
            （1024 进制、单位是 `KiB`/`MiB`、非字节时两位小数）⇒ 例子应是
            `↓ 1.20 MiB/s ↑ 34.00 KiB/s`，而不是 `MB/s`/`KB/s`。
          菜单栏那一半是**对的**（`traffic.rs` 的 `tray_title` → `format_rate_compact`
          给 `↓1.2M ↑34K`，且速率为 0 时整串留空 ⇒ 「空闲时不显示」），保留。
        */}
        {/*
          这段话**按开关的真值分两句**（task-140 后半段）：`show_speed_in_title` 关着时，
          顶栏那块速率根本不渲染（`App.tsx:224` `showSpeed = show_speed_in_title && running`），
          菜单栏也会被清空（`traffic.rs:130-135` `tray.set_title(Some(""))`）——
          所以关着的时候不能再用现在时描述「顶栏显示 ↓…」。
        */}
        <div className="field__hint" style={{ marginBottom: 10 }}>
          {settings.show_speed_in_title ? (
            <>
              现在开着：<strong>顶栏</strong>（App 自己画的那条 —— <strong>不是</strong> macOS
              原生标题栏，原生标题被隐藏了，去标题栏找是找不到的）显示{" "}
              <span className="mono">↓ 1.20 MiB/s ↑ 34.00 KiB/s</span>；
            </>
          ) : (
            <>
              现在<strong>关着</strong>：顶栏不再带速率（只留页面标题），菜单栏里的读数也会被清空。
              打开之后：<strong>顶栏</strong>（App 自己画的那条 —— <strong>不是</strong> macOS
              原生标题栏，原生标题被隐藏了，去标题栏找是找不到的）显示{" "}
              <span className="mono">↓ 1.20 MiB/s ↑ 34.00 KiB/s</span>；
            </>
          )}
          菜单栏因为要和系统图标抢地方，用更短的{" "}
          <span className="mono">↓1.2M ↑34K</span>，且空闲（速率为 0）时不显示。
          速率来自核心的流量计数器，系统代理与 TUN 两种模式都有效；
          原生窗口标题也在同步更新（所以「窗口」菜单与 Mission Control 的窗口列表里带速率），
          但标题栏上仍然看不到它。
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
          {/*
            task-131（顺带修 task-128 点名的行为级缺陷）：原来是
            `navigator.clipboard.writeText(text).catch(() => console.log(text))`
            —— 剪贴板被拒时界面**毫无反应**，用户以为复制成功、贴出去是空的。
            现在走共用的 `CopyButton`：成功有 `role="status"` 提示，
            失败给 `role="alert"` + 一个可手动选中的文本区。
          */}
          <CopyButton
            label="复制诊断报告"
            disabled={busy !== null}
            load={() => api.diagnostics()}
          />
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
