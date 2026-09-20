/**
 * 地球仪：本机 → 出口节点的航线。
 *
 * # 画的是什么
 *
 * 一条从本机公网出口到出口节点的**大圆弧**（球面上两点间的最短路径，也正是
 * 真实网络包大致会走的方向），飞机沿弧线飞；弧线两端各有一个位置点。
 *
 * 飞机的**数量**由这条航线的**累计流量**决定（`vehicleCount(route.bytes)`）——
 * v0.8.22 起 `route.bytes` 是「跨核心重启单调的累计值」，**不是当前速率**；
 * 飞的**快慢**根本不是数据驱动的，而是固定的视觉节奏（见下面的「哪些是真的」）。
 *
 * # 哪些是真的，哪些不是
 *
 * * 位置：**真**的，但来自 ip-api.com（项目自带的 geoip.dat 没有经纬度）。
 *   界面上标注来源，因为这意味着「被查的 IP 发给了第三方」。
 * * 航线弧度：**真**的几何（两点确定的大圆）。
 * * 大陆轮廓：**粗略**。2° 分辨率的海陆位图（约 4KB，不引地图依赖），
 *   不是导航级海岸线 —— 方位感是对的，细节没有。
 * * 飞行速度：**视觉节奏**，不代表真实时延。
 *
 * # 为什么自己画而不引 Three.js
 *
 * 前端目前只有 React + Tauri（包体 195KB / gzip 66KB）。一个地球仪不值得
 * 让包体翻倍 —— 正交投影 + 大圆插值这些数学量很小，画布绘制也够用。
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { api, errorText } from "../ipc";
import { LAND_MASK_HEX, decodeLandMaskFlat, sampleLand } from "../landmask";
import { formatBytes } from "../types";
import type { GeoLocation, GlobeData } from "../types";

const DEG = Math.PI / 180;

/** 经纬度 → 单位球面坐标（z 轴指向观察者）。 */
function toVec(lat: number, lon: number): [number, number, number] {
  const la = lat * DEG;
  const lo = lon * DEG;
  return [Math.cos(la) * Math.sin(lo), Math.sin(la), Math.cos(la) * Math.cos(lo)];
}

/** 绕 Y 轴（经度方向）旋转。 */
function rotY(v: [number, number, number], a: number): [number, number, number] {
  const [x, y, z] = v;
  const c = Math.cos(a);
  const s = Math.sin(a);
  return [x * c + z * s, y, -x * s + z * c];
}

/** 绕 X 轴（纬度方向）旋转 —— 拖动时用。 */
function rotX(v: [number, number, number], a: number): [number, number, number] {
  const [x, y, z] = v;
  const c = Math.cos(a);
  const s = Math.sin(a);
  return [x, y * c - z * s, y * s + z * c];
}

/** 球面上两点之间插值（大圆的近似：球面线性插值，视觉上足够）。 */
function slerp(
  a: [number, number, number],
  b: [number, number, number],
  t: number,
): [number, number, number] {
  const dot = Math.max(-1, Math.min(1, a[0] * b[0] + a[1] * b[1] + a[2] * b[2]));
  const omega = Math.acos(dot);
  if (omega < 1e-6) return a;
  const so = Math.sin(omega);
  const ca = Math.sin((1 - t) * omega) / so;
  const cb = Math.sin(t * omega) / so;
  return [a[0] * ca + b[0] * cb, a[1] * ca + b[1] * cb, a[2] * ca + b[2] * cb];
}

/** 两点间的大圆角距（弧度）—— 决定弧线拱多高。 */
function angularDistance(
  a: [number, number, number],
  b: [number, number, number],
): number {
  const dot = Math.max(-1, Math.min(1, a[0] * b[0] + a[1] * b[1] + a[2] * b[2]));
  return Math.acos(dot);
}

export default function Globe() {
  const [data, setData] = useState<GlobeData | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    setBusy(true);
    try {
      setData(await api.globeData());
      setLoadError(null);
    } catch (e) {
      setLoadError(errorText(e));
    } finally {
      setBusy(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  return (
    <div className="page">
      <section className="page__sec">
        <div className="row row--between">
          <div>
            <h2 className="page__title">地球仪</h2>
            <p className="page__desc">
              从本机到出口节点的大圆弧航线。飞机数量由这条航线的<strong>累计流量</strong>决定
              （累计值，不代表当前速率）；飞行快慢是视觉节奏，与数据无关。
              大陆轮廓是 2° 分辨率的粗略示意，不是导航级海岸线。
            </p>
          </div>
          <button className="btn btn--ghost" disabled={busy} onClick={() => void load()}>
            {busy ? <span className="spin" /> : null}
            重新定位
          </button>
        </div>

        {loadError && (
          <div className="banner banner--error">
            <span>✕</span>
            <div>{loadError}</div>
          </div>
        )}

        <GlobeCanvas data={data} />

        {data?.error && (
          <div className="note">
            {data.error}
            <br />
            位置查询需要访问 ip-api.com（项目自带的 geoip 数据没有经纬度）。
          </div>
        )}
        {data?.route && <RouteFacts data={data} />}
      </section>
    </div>
  );
}

/** 航线两端的事实卡片。 */
function RouteFacts({ data }: { data: GlobeData }) {
  const r = data.route!;
  const km = greatCircleKm(r.from, r.to);
  return (
    <div className="facts">
      <Fact label="起点" loc={r.from} />
      <div className="facts__mid">
        <div className="facts__km">{Math.round(km).toLocaleString()} km</div>
        <div className="facts__hint">大圆距离</div>
        <div className="facts__km facts__km--small">{formatBytes(r.bytes)}</div>
        <div className="facts__hint">出口累计流量（实测）</div>
      </div>
      <Fact label={`出口 · ${r.node_name}`} loc={r.to} />
    </div>
  );
}

function Fact({ label, loc }: { label: string; loc: GeoLocation }) {
  return (
    <div className="fact">
      <div className="fact__label">{label}</div>
      <div className="fact__place">
        {loc.city ? `${loc.city} · ` : ""}
        {loc.country || "未知"}
      </div>
      <div className="fact__meta">
        {loc.ip} · {loc.lat.toFixed(2)}, {loc.lon.toFixed(2)}
      </div>
      {loc.isp && <div className="fact__meta">{loc.isp}</div>}
      {/* IP 地理定位是**尽力而为**：运营商大内网 / 省级骨干出口会让注册地
          偏离实际城市。多个数据源结果不一致时必须说明，否则用户会以为
          那是确定位置。 */}
      {loc.consistent === false && (
        <div className="fact__warn">数据源判定不一致，仅按 IP 归属估算</div>
      )}
      <div className="fact__src">
        位置来源：{loc.sources.length > 0 ? loc.sources.join(" · ") : loc.source}
      </div>
    </div>
  );
}

/**
 * 缩放范围。
 *
 * `1` 是整球可见的基准（球半径 = 画布短边的 0.42）。
 *
 * **上限 40**（2026-09 从 8 提到 40）。为什么必须这么高：广州↔香港只有
 * **1.163°** 角距，正交投影下两点的屏幕距离是 `2R·sin(θ/2)`，`R = 0.42·720·zoom`；
 * zoom=8 时只有 **38 CSS px**，两个标记点、航线、飞机全糊成一团。要拉开到
 * 约 200px 需要 zoom ≈ 41.9，所以取 40。
 *
 * **代价必须说清**（也写进了 `docs/ui/globe/README.md`）：大陆轮廓是 2° 分辨率
 * 的海陆位图（约 200 公里一格），放大到 30–40× 时海岸线会明显发虚 —— 它给的是
 * 方位感，不是地图精度。提高位图分辨率（1° 约 16KB）是后续可选项，本轮没做。
 *
 * 下限 0.55：跨半球时能把两端一起收进画面。
 */
export const MIN_ZOOM = 0.55;
export const MAX_ZOOM = 40;

/**
 * 近处航线默认要拉开到的目标角距（弧度）：0.7 rad ≈ 40°。
 *
 * 含义是「两端之间的角距在默认视角里大约占 40°」——换算到 720px 画布上，
 * 两点的屏幕距离约 200px，看得出是从哪飞到哪，而不是一个点。
 */
const TARGET_SPAN_RAD = 0.7;

export function clampZoom(z: number): number {
  return Math.max(MIN_ZOOM, Math.min(MAX_ZOOM, z));
}

/**
 * 默认缩放：**既保证装得下，也保证两端分得开**。
 *
 * 正交投影下两点夹角 θ 的屏幕距离是 `2R·sin(θ/2)`。这里有两个相反的诉求：
 *
 * * **装得下**（上限）：跨半球时两端要同时进画面 —— 这是老逻辑，
 *   `sin(θ/2)` 超过 0.7 就得缩小，且不超过 1。
 * * **分得开**（下限）：近距离航线（广州↔香港 1.163°）在老逻辑下恒为 1，
 *   屏幕上只有 **4.9 CSS px** —— 两个标记、航线、飞机全叠在一起，
 *   用户说的「放大倍数不够」就是这个。所以要按「把 θ 拉到 TARGET_SPAN_RAD」
 *   来放大：`zoom = TARGET_SPAN_RAD / θ`。
 *
 * 两者取「需要更大的那个」：近处用放大值，远处用缩小值，交接处（θ≈40°）
 * 恰好都是 1，所以是连续的。两点重合时没有航线可言，保持整球可见。
 */
export function fitZoom(data: GlobeData): number {
  const r = data.route;
  if (!r) return 1;
  const a = toVec(r.from.lat, r.from.lon);
  const b = toVec(r.to.lat, r.to.lon);
  const theta = angularDistance(a, b);
  if (!(theta > 1e-6)) return 1; // 两点重合：不放大（也避免除以零）
  // 装得下所需（≤1）
  const fit = Math.min(1, 0.7 / Math.sin(theta / 2));
  // 分得开所需（>1 表示要放大）
  const want = TARGET_SPAN_RAD / theta;
  return clampZoom(want > 1 ? want : fit);
}

/**
 * 初始/复位视角：对准哪里、缩放多少、**是否允许自转**。
 *
 * # 为什么放大时必须关闭自转
 *
 * 整球可见时自转只是让球慢慢转动，观感好。但放大之后视野张开角很小，
 * 自转会把整个视野扫走 —— 实测：zoom=34.5（R=10424）时自转 13.5° 让两点
 * 横向偏出 **2500px**，整屏空白、标记与航线全在画布外。
 *
 * 判据是两点跨过的角度：跨得越小、需要放得越大、自转的破坏越强。
 * `ROTATE_MAX_SPAN_RAD` 约 3.4°，对应 zoom≈11。
 */
export const ROTATE_MAX_SPAN_RAD = 0.06;

export function viewFor(data: GlobeData): {
  lat: number;
  lon: number;
  zoom: number;
  auto: boolean;
} | null {
  const focus = focusPoint(data);
  if (!focus) return null;
  const r = data.route;
  // 没有航线时当作「看整球」，此时自转无妨
  const span = r
    ? angularDistance(toVec(r.from.lat, r.from.lon), toVec(r.to.lat, r.to.lon))
    : Math.PI;
  return {
    // 居中：`rotX(+φ)` 把焦点转到正前方（数值验证 (0,0,1)）；经度取负。
    // 早先纬度乘了 0.6，zoom=1 时只偏 48px 看不出，放大到 34.5× 后焦点
    // 被推到画布上方 1287px —— 整屏空白。**不要乘系数。**
    lat: focus.lat * DEG,
    lon: -focus.lon * DEG,
    zoom: fitZoom(data),
    auto: span >= ROTATE_MAX_SPAN_RAD,
  };
}

/**
 * 视角该对准哪一点。
 *
 * `bytes` 是**整条航线**承载的量，并不属于某一端 —— 所以有流量时对准
 * **两端的中点**（那就是「让人处在比较合适的观看位置」），没有流量数据时
 * 退到起点（本机）。
 *
 * 导出是为了能测：这属于「算错的后果是视角偏到看不见航线」的那类逻辑，
 * 不该只靠截图眼看。
 */
export function focusPoint(data: GlobeData): { lat: number; lon: number } | null {
  const r = data.route;
  if (r && r.bytes > 0) {
    return { lat: (r.from.lat + r.to.lat) / 2, lon: (r.from.lon + r.to.lon) / 2 };
  }
  const p = r ? r.from : data.origin;
  return p ? { lat: p.lat, lon: p.lon } : null;
}
/** 两点大圆距离（公里）。 */
function greatCircleKm(a: GeoLocation, b: GeoLocation): number {
  const R = 6371;
  const dLat = (b.lat - a.lat) * DEG;
  const dLon = (b.lon - a.lon) * DEG;
  const s =
    Math.sin(dLat / 2) ** 2 +
    Math.cos(a.lat * DEG) * Math.cos(b.lat * DEG) * Math.sin(dLon / 2) ** 2;
  return 2 * R * Math.asin(Math.min(1, Math.sqrt(s)));
}


/**
 * 当前视角的坐标变换。
 *
 * **陆地与航线必须共用这一个变换** —— 各写一份的话，稍微改动一处旋转顺序，
 * 航线就会整体偏离大陆，而且看起来「像是对的」。所以投影只在这里实现一次。
 */
function makeProjection(W: number, H: number, rotLon: number, rotLat: number, zoom = 1) {
  const cx = W / 2;
  const cy = H / 2;
  // zoom 直接乘在半径上：球体渲染（逐像素）与航线投影共用这一个 R，
  // 所以缩放对两者天然一致，不会出现「球放大了但航线没跟上」。
  const R = Math.min(W, H) * 0.42 * zoom;

  /** 单位球面点 → 屏幕（含深度 z，>0 表示朝向观察者）。 */
  const project = (v: [number, number, number]): [number, number, number] => {
    const r = rotX(rotY(v, rotLon), rotLat);
    return [cx + r[0] * R, cy - r[1] * R, r[2]];
  };

  /** 屏幕上的一点 → 球面法线（用于逐像素采样）；不在球上返回 null。 */
  const unproject = (sx: number, sy: number): [number, number, number] | null => {
    const x = (sx - cx) / R;
    const y = (cy - sy) / R;
    const d2 = x * x + y * y;
    if (d2 > 1) return null;
    const z = Math.sqrt(1 - d2);
    return rotY(rotX([x, y, z], -rotLat), -rotLon);
  };

  return { cx, cy, R, project, unproject };
}

/** 球体画布。 */
function GlobeCanvas({ data }: { data: GlobeData | null }) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  // 视角放在 ref 而不是 state：动画每帧都在改它，放 state 会让 effect
  // 每帧重建一次 RAF 循环（还把 drawScene 的闭包全换掉），纯属浪费。
  const view = useRef({ lon: 0, lat: 0.25, zoom: 1, auto: true });
  const drag = useRef<{ x: number; y: number } | null>(null);

  // 初始视角对准**流量最大**的那一端，并把距离调到两端刚好都在视野里。
  //
  // 为什么是「最大」而不是固定的本机：用户关心的是流量去了哪儿。
  // 两地相距很远时（跨半球）需要缩小才能同时看到两端 —— 那个距离就是
  // 「最合适的观看位置」，而不是固定一个缩放值。
useEffect(() => {
      const v = data ? viewFor(data) : null;
      if (!v) return;
      view.current.lon = v.lon;
      view.current.lat = v.lat;
      view.current.zoom = v.zoom;
      view.current.auto = v.auto;
    }, [data]);

  const land = useMemo(() => decodeLandMaskFlat(LAND_MASK_HEX), []);

  // 动画循环：只依赖数据本身，不依赖视角
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    let raf = 0;
    const start = performance.now();

    const draw = (now: number) => {
      const t = (now - start) / 1000;
      // 拖动过就不要自动转，否则会跟用户抢
      if (view.current.auto) view.current.lon += 0.0016;
      drawScene(ctx, canvas, land, data, view.current.lon, view.current.lat, view.current.zoom, t);
      raf = requestAnimationFrame(draw);
    };
    raf = requestAnimationFrame(draw);
    return () => cancelAnimationFrame(raf);
  }, [land, data]);

  const onDown = (e: React.PointerEvent) => {
    view.current.auto = false;
    drag.current = { x: e.clientX, y: e.clientY };
    (e.target as Element).setPointerCapture?.(e.pointerId);
  };
  const onMove = (e: React.PointerEvent) => {
    if (!drag.current) return;
    const dx = e.clientX - drag.current.x;
    const dy = e.clientY - drag.current.y;
    drag.current = { x: e.clientX, y: e.clientY };
    view.current.lon += dx * 0.006;
    view.current.lat = Math.max(-1.3, Math.min(1.3, view.current.lat + dy * 0.006));
  };
  const onUp = () => {
    drag.current = null;
  };
  // 滚轮缩放：向上滚放大。用非 passive 监听才能 preventDefault，
  // 否则页面会跟着一起滚。
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      view.current.zoom = clampZoom(view.current.zoom * (e.deltaY < 0 ? 1.12 : 1 / 1.12));
    };
    canvas.addEventListener("wheel", onWheel, { passive: false });
    return () => canvas.removeEventListener("wheel", onWheel);
  }, []);

  return (
    <div className="globe">
      <canvas
        ref={canvasRef}
        width={720}
        height={720}
        className="globe__canvas"
        onPointerDown={onDown}
        onPointerMove={onMove}
        onPointerUp={onUp}
        onPointerLeave={onUp}
      />
      <div className="globe__tools">
        <button
          className="globe__tool"
          title="放大"
          onClick={() => {
            view.current.auto = false;
            view.current.zoom = clampZoom(view.current.zoom * 1.25);
          }}
        >
          ＋
        </button>
        <button
          className="globe__tool"
          title="缩小"
          onClick={() => {
            view.current.auto = false;
            view.current.zoom = clampZoom(view.current.zoom / 1.25);
          }}
        >
          －
        </button>
        <button
          className="globe__tool globe__tool--wide"
          title="复位到默认视角"
          onClick={() => {
            const v = data ? viewFor(data) : null;
            if (v) {
              view.current.lon = v.lon;
              view.current.lat = v.lat;
              view.current.zoom = v.zoom;
              view.current.auto = v.auto;
            } else {
              view.current.zoom = 1;
              view.current.auto = true;
            }
          }}
        >
          复位
        </button>
      </div>
      <div className="globe__hint">{`滚轮缩放（最多 ${MAX_ZOOM}×）· 拖动旋转`}</div>
    </div>
  );
}

/** 画一帧。 */
function drawScene(
  ctx: CanvasRenderingContext2D,
  canvas: HTMLCanvasElement,
  land: number[],
  data: GlobeData | null,
  rotLon: number,
  rotLat: number,
  zoom: number,
  t: number,
) {
  const W = canvas.width;
  const H = canvas.height;
  const { cx, cy, R, project, unproject } = makeProjection(W, H, rotLon, rotLat, zoom);

  ctx.clearRect(0, 0, W, H);

  // ---- 球体：逐像素采样陆地 ----
  //
  // 为什么不用「把每个格子投影成方块」：那样会留缝、海岸线是方的。
  // 逐像素反向投影 + 双线性采样能得到平滑的海岸线，代价是每帧约
  // 50 万次坐标运算 —— 对这些运算量来说完全可以接受。
  const img = ctx.createImageData(W, H);
  const px = img.data;
  const x0 = Math.max(0, Math.floor(cx - R) - 1);
  const x1 = Math.min(W - 1, Math.ceil(cx + R) + 1);
  const y0 = Math.max(0, Math.floor(cy - R) - 1);
  const y1 = Math.min(H - 1, Math.ceil(cy + R) + 1);
  const R2 = R * R;

  // 光照方向（左上），让球面有立体感
  const L = [-0.45, 0.55, 0.7];

  for (let y = y0; y <= y1; y++) {
    for (let x = x0; x <= x1; x++) {
      const dx = x - cx;
      const dy = y - cy;
      const d2 = dx * dx + dy * dy;
      const i = (y * W + x) * 4;
      if (d2 > R2) continue; // 球外：留透明

      const nx = dx / R;
      const ny = -dy / R;
      const nz = Math.sqrt(Math.max(0, 1 - d2 / R2));

      const lit = Math.max(0, nx * L[0]! + ny * L[1]! + nz * L[2]!);
      const rim = 1 - nz; // 边缘压暗，像一颗球

      const world = unproject(x, y);
      let landAmount = 0;
      if (world) {
        const lat = Math.asin(Math.max(-1, Math.min(1, world[1]))) / DEG;
        const lon = Math.atan2(world[0], world[2]) / DEG;
        landAmount = sampleLand(land, lat, lon);
      }

      // 海洋
      let r = 14 + 10 * lit;
      let g = 22 + 14 * lit;
      let b = 38 + 22 * lit;

      if (landAmount > 0.5) {
        // 陆地：偏灰绿，同样受光照
        const t2 = (0.5 + lit * 0.5) * 0.95;
        r = 62 * t2 + 14;
        g = 92 * t2 + 20;
        b = 116 * t2 + 30;
      } else if (landAmount > 0.12) {
        // 海岸线过渡：把插值区间的中间地带稍微提亮，形成一圈浅滩
        const k = (landAmount - 0.12) / 0.38;
        r += 18 * k * lit;
        g += 26 * k * lit;
        b += 30 * k * lit;
      }

      const shade = 1 - rim * 0.35;
      px[i] = Math.min(255, r * shade);
      px[i + 1] = Math.min(255, g * shade);
      px[i + 2] = Math.min(255, b * shade);
      px[i + 3] = 255; // 球体本身不透明
    }
  }
  ctx.putImageData(img, 0, 0);

  // ---- 经纬网：极淡的参考线，像海图 ----
  ctx.strokeStyle = "rgba(120, 160, 210, 0.10)";
  ctx.lineWidth = 1;
  for (let lat = -60; lat <= 60; lat += 30) {
    ctx.beginPath();
    let started = false;
    for (let lon = -180; lon <= 180; lon += 4) {
      const [sx, sy, pz] = project(toVec(lat, lon));
      if (pz <= 0) {
        started = false;
        continue;
      }
      if (!started) {
        ctx.moveTo(sx, sy);
        started = true;
      } else ctx.lineTo(sx, sy);
    }
    ctx.stroke();
  }
  for (let lon = -180; lon < 180; lon += 30) {
    ctx.beginPath();
    let started = false;
    for (let lat = -90; lat <= 90; lat += 4) {
      const [sx, sy, pz] = project(toVec(lat, lon));
      if (pz <= 0) {
        started = false;
        continue;
      }
      if (!started) {
        ctx.moveTo(sx, sy);
        started = true;
      } else ctx.lineTo(sx, sy);
    }
    ctx.stroke();
  }

  // ---- 球体轮廓（大气辉光） ----
  ctx.beginPath();
  ctx.arc(cx, cy, R, 0, Math.PI * 2);
  ctx.strokeStyle = "rgba(79, 142, 247, 0.30)";
  ctx.lineWidth = 1.2;
  ctx.stroke();

  // ---- 航线 ----
  const route = data?.route;
  if (route) {
    const a = toVec(route.from.lat, route.from.lon);
    const b = toVec(route.to.lat, route.to.lon);
    const arc = angularDistance(a, b);

    // 大圆抬升：跨得越远拱得越高（近处两点几乎贴地，远处才高高拱起）
    const lift = 0.04 + 0.20 * (arc / Math.PI);
    const arcPoint = (u: number): [number, number, number] => {
      const p = slerp(a, b, u);
      const scale = 1 + lift * Math.sin(Math.PI * u);
      return project([p[0] * scale, p[1] * scale, p[2] * scale]);
    };

    ctx.beginPath();
    for (let i = 0; i <= 160; i++) {
      const [sx, sy, pz] = arcPoint(i / 160);
      // 地平线以下的部分不画（否则会浮在球外）
      if (pz < -0.05) continue;
      if (i === 0) ctx.moveTo(sx, sy);
      else ctx.lineTo(sx, sy);
    }
    ctx.strokeStyle = "rgba(79, 142, 247, 0.6)";
    ctx.lineWidth = 1.6;
    ctx.stroke();

    // 飞机：数量由实测字节决定
    const planes = vehicleCount(route.bytes);
    for (let i = 0; i < planes; i++) {
      const u = (t / 7 + i / planes) % 1;
      const [sx, sy, pz] = arcPoint(u);
      if (pz <= 0) continue; // 绕到背面就藏起来
      drawPlane(ctx, sx, sy, arcPoint, u);
    }

    // 标记上写**具体地点**（城市 + 国别），而不是「本机」这种看不出去哪儿的词；
    // 第二行小字给 IP。城市的缺失（有些 IP 查不到城市）用国别兜底。
    const placed: { x: number; y: number; w: number; h: number }[] = [];
    marker(ctx, project, route.from, "#34d399", placeLabel(route.from), "本机 · " + route.from.ip, placed);
    marker(ctx, project, route.to, "#4f8ef7", placeLabel(route.to), route.node_name, placed);
  }
}

/** 飞机数量：按实测字节，每 512KB 一架，最多 6 架。 */
function vehicleCount(bytes: number): number {
  if (bytes <= 0) return 1;
  return Math.max(1, Math.min(6, Math.floor(bytes / (512 * 1024)) || 1));
}

/** 画一架飞机（按航向旋转的小三角 + 尾迹）。 */
function drawPlane(
  ctx: CanvasRenderingContext2D,
  px: number,
  py: number,
  arcPoint: (u: number) => [number, number, number],
  u: number,
) {
  // 航向由弧线上前后两点决定
  const ahead = arcPoint(Math.min(1, u + 0.01));
  const ang = Math.atan2(ahead[1] - py, ahead[0] - px);

  ctx.save();
  ctx.translate(px, py);
  ctx.rotate(ang);

  // 尾迹
  const trail = ctx.createLinearGradient(-22, 0, 0, 0);
  trail.addColorStop(0, "rgba(79, 142, 247, 0)");
  trail.addColorStop(1, "rgba(140, 190, 255, 0.55)");
  ctx.strokeStyle = trail;
  ctx.lineWidth = 2;
  ctx.beginPath();
  ctx.moveTo(-22, 0);
  ctx.lineTo(-4, 0);
  ctx.stroke();

  // 机身
  ctx.fillStyle = "#dbeafe";
  ctx.beginPath();
  ctx.moveTo(7, 0);
  ctx.lineTo(-4, 4.2);
  ctx.lineTo(-1.5, 0);
  ctx.lineTo(-4, -4.2);
  ctx.closePath();
  ctx.fill();

  ctx.restore();
}

/** 画一个位置标记（点 + 标签 + 呼吸圈）。 */
function marker(
  ctx: CanvasRenderingContext2D,
  project: (v: [number, number, number]) => [number, number, number],
  loc: GeoLocation,
  color: string,
  /** 主行：具体地点（城市 + 国别）。 */
  label: string,
  /** 副行：小字补充（IP 或节点名）。 */
  sub?: string,
  /** 已占用的标签框，用于避让（相邻地点只差一百多公里时标签会叠在一起）。 */
  placed?: { x: number; y: number; w: number; h: number }[],
) {
  const [px, py, pz] = project(toVec(loc.lat, loc.lon));
  if (pz <= 0.02) return; // 背面不画（否则会浮在球外，看着像错位）

  const pulse = 1 + 0.35 * Math.sin(performance.now() / 500);
  ctx.beginPath();
  ctx.arc(px, py, 7 * pulse, 0, Math.PI * 2);
  ctx.strokeStyle = color;
  ctx.globalAlpha = 0.35;
  ctx.lineWidth = 1.5;
  ctx.stroke();
  ctx.globalAlpha = 1;

  ctx.beginPath();
  ctx.arc(px, py, 4, 0, Math.PI * 2);
  ctx.fillStyle = color;
  ctx.fill();

  // 标签底衬：球面有明暗，纯文字在某些区域会看不清
  const main = `600 13px -apple-system, "PingFang SC", ui-monospace, sans-serif`;
  const under = `400 10.5px -apple-system, "PingFang SC", ui-monospace, sans-serif`;
  ctx.textAlign = "left";
  const tx = px + 11;

  ctx.font = main;
  const w1 = ctx.measureText(label).width;
  const w2 = sub ? (ctx.font = under, ctx.measureText(sub).width) : 0;
  const boxW = Math.max(w1, w2) + 10;
  const boxH = sub ? 30 : 18;
  let by = py - (sub ? 15 : 10);

  // 避让：与已放好的标签太近就往下挪，直到不撞。
  // 两个地点只差一百多公里时（例如广州与香港），标签本来就该看得到两个。
  if (placed) {
    const overlaps = (y: number) =>
      placed.some(
        (p) =>
          Math.abs(p.x - (tx - 5)) < Math.max(p.w, boxW) &&
          Math.abs(p.y - y) < (p.h + boxH) / 2 + 2,
      );
    let guard = 0;
    while (overlaps(by) && guard < 8) {
      by += boxH + 5;
      guard++;
    }
  }
  const box = { x: tx - 5, y: by, w: boxW, h: boxH };
  placed?.push(box);

  // 从标记点到标签的引线：挪开之后仍然看得出标签对应哪个点
  ctx.strokeStyle = "rgba(160, 178, 200, 0.45)";
  ctx.lineWidth = 1;
  ctx.beginPath();
  ctx.moveTo(px + 4, py);
  ctx.lineTo(box.x, by + boxH / 2);
  ctx.stroke();

  ctx.fillStyle = "rgba(8, 12, 20, 0.62)";
  ctx.beginPath();
  ctx.roundRect(box.x, by, boxW, boxH, 4);
  ctx.fill();

  ctx.font = main;
  ctx.fillStyle = "rgba(236, 242, 250, 0.96)";
  ctx.fillText(label, tx, by + 13);

  if (sub) {
    ctx.font = under;
    ctx.fillStyle = "rgba(160, 178, 200, 0.9)";
    ctx.fillText(sub, tx, by + 26);
  }
}

/**
 * 位置标记的主行文字：**具体地点**。
 *
 * 用户要的是「看到香港、大理」，所以城市优先；查不到城市时退到国别，
 * 两个都没有才说「未知位置」—— 不留空，也不编。
 */
function placeLabel(loc: GeoLocation): string {
  const city = loc.city.trim();
  const country = loc.country.trim();
  if (city && country) return `${city} · ${country}`;
  return city || country || "未知位置";
}
