/**
 * 海陆位图（180×90，2°×2°）—— 地球仪的大陆轮廓。
 *
 * # 为什么不用 GeoJSON
 *
 * 画真实海岸线需要一份世界地图数据（GeoJSON 通常 100KB+，压缩后也几十 KB）。
 * 这一页只是给人看「流量从哪飞到哪」的全景，不需要导航级精度，而前端目前
 * 只有 React 与 Tauri 两个依赖（包体 195KB/gzip 66KB），为此翻倍不划算。
 *
 * 所以用一张 **2° 分辨率的海陆位图**：每 4 格压成一个十六进制字符，整张图
 * 约 4KB。轮廓是按各大洲真实经纬范围勾的多边形生成的（不是随手画的），
 * 陆地占比 30.4%（地球实际约 29%），在球面上能认出各块大陆。
 *
 * 代价要如实承认：**这是粗略轮廓**，海岸线是方的、小岛屿基本没有。
 * 它表达的是方位感，不是地理精度。
 */

/** 180×90 位图，每字符 4 格（高位在前），1=陆地。 */
export const LAND_MASK_HEX =
  "000000000000000000000000000000000000000000000" +
  "000000000000000000000000000000000000000000000" +
  "000000000000000000000000000000000000000000000" +
  "00000000000000003fc00000000000000000000000000" +
  "0000000000000003ffff0000000000000000000000000" +
  "000000000000000fffff0000000000000000000000000" +
  "0000000000000007ffff0000000000000000000000000" +
  "0000000000000007ffff000000000000007fff0000000" +
  "0000000000000007ffff0000000000003fffffff00000" +
  "00000000fffff003ffff000000000000ffffffffff000" +
  "00fffffffffff803fffe00001f000001ffffffffffff0" +
  "01fffffffffffe01fff000003f000007fffffffffffff" +
  "01ffffffffffff00ffc1e000ff00000ffffffffffffff" +
  "003fffffffffffc03e000001ff80003ffffffffffffff" +
  "0007ffffffffffe018000003ff80007ffffffffffffff" +
  "000007fffffffff000000007ff7fffffffffffffffffe" +
  "000001fffffffff8000001fffc7fffffffffffffffff0" +
  "0000007ffffffff8000001fff07ffffffffffffffffc0" +
  "0000007ffffffffc000001f8003ffffffffffffffff00" +
  "0000003ffffffffc000003e0003fffffffffffffff800" +
  "0000003ffffffff8000003c0003ffffffffffffffe000" +
  "0000003fffffffe000000300003ffffffffffffff8000" +
  "0000001fffffff8000000200003fffffffffffffc0000" +
  "0000001fffffff0000000000003fffffffffffffc0000" +
  "0000001ffffffe0000000000001fffffffffffffc0000" +
  "0000000ffffffc0000000000001ffffffffffffd80000" +
  "00000007fffff80000000000001ffffffffffff300000" +
  "00000003fffff000000001ffc01fffffffffffe200000" +
  "00000001fffff000000003fff81fffffffffff8000000" +
  "00000000ffffe000000007ffff9fffffffffff0000000" +
  "000000007fffe00000000ffffffffffffffffc0000000" +
  "000000003fffc00000001ffffffffffffffff80000000" +
  "000000001f80000000003fffffffffffffffe00000000" +
  "000000000e00000000003ffffffbfe3fffffc00000000" +
  "000000000403f00000003ffffffbf807ffff800000000" +
  "000000000007fc0000003ffffffdf003fdff800000000" +
  "000000000007fe0000003ffffffde001fdfe000000000" +
  "00000000000ffc0000007ffffffec001fdfc000000000" +
  "000000000007f80000003ffffffe0001fcf8000000000" +
  "000000000001f80000001fffffffe000fcf0000000000" +
  "000000000000fff800000fffffffe000fc60000000000" +
  "0000000000007ffc000007ffffffc0006040000000000" +
  "0000000000007ffe000003ffffff80000000000000000" +
  "0000000000007fff00000001ffff00000000000000000" +
  "0000000000007fff80000001ffff00000020000000000" +
  "0000000000007fffc0000001fffe00000018000000000" +
  "0000000000007ffff8000001fffe0000000e000000000" +
  "0000000000007fffff000001fffe000000078007c0000" +
  "0000000000003fffff000001fffe00000003e001e0000" +
  "0000000000001fffff000001fffc0000000000c078000" +
  "0000000000001fffff000000fffc00000000000000000" +
  "0000000000000ffffe000000fffc00000000003fc0000" +
  "00000000000007fffe000000fffc20000000007fc0000" +
  "00000000000007fffc000000fff86000000000ffe0000" +
  "00000000000003fff8000000fff8c000000003ffe0000" +
  "00000000000003fff00000007ff0800000000ffff0000" +
  "00000000000003ffe00000007ff0000000001ffff8000" +
  "00000000000003ffc00000007fe0000000001ffff8000" +
  "00000000000003ff800000003fc0000000001ffffc000" +
  "00000000000003ff000000003f80000000001ffffc000" +
  "00000000000003fe000000003f00000000001ffffc000" +
  "00000000000003fc000000001e00000000001e1ffc000" +
  "00000000000003f8000000000000000000000007f8004" +
  "00000000000003f0000000000000000000000001f800e" +
  "00000000000003f00000000000000000000000000000c" +
  "00000000000007e000000000000000000000000000008" +
  "00000000000007e000000000000000000000000000000" +
  "00000000000007c000000000000000000000000000000" +
  "000000000000078000000000000000000000000000000" +
  "000000000000078000000000000000000000000000000" +
  "000000000000070000000000000000000000000000000" +
  "000000000000000000000000000000000000000000000" +
  "000000000000000000000000000000000000000000000" +
  "000000000000000000000000000000000000000000000" +
  "000000000000000000000000000000000000000000000" +
  "000000000000000000000000000000000000000000000" +
  "000000000000000000000000000000000000000000000" +
  "000000000000007f80000000000000000000000000000" +
  "0000000000001fffffe00000000000000000000000000" +
  "000000000007fffffffff8000fffffffc000000000000" +
  "0000000001fffffffffffffffffffffffff0000000000" +
  "000000007fffffffffffffffff80000000000000003ff" +
  "00000000000000000000000000000000000003fffffff" +
  "000000000000000000000000000000003ffffffffffff" +
  "0000000000000000000000000003fffffffffffffffff" +
  "00000000000000000000003ffffffffffffffffffffff" +
  "000000000000000003fffffffffffffffffffffffffff" +
  "0000000000003ffffffffffffffffffffffffffffffff" +
  "00000003fffffffffffffffffffffffffffffffffffff" +
  "003ffffffffffffffffffffffffffffffffffffffffff";

/** 位图宽度（经度方向格数）。 */
export const MASK_W = 180;
/** 位图高度（纬度方向格数）。 */
export const MASK_H = 90;

/** 解码成 `[row][col]` 的布尔表。只解一次，调用方缓存结果。 */
export function decodeLandMask(hex: string): boolean[][] {
  const rows: boolean[][] = [];
  let bitIndex = 0;
  const total = MASK_W * MASK_H;
  let row: boolean[] = [];
  for (let i = 0; i < hex.length && bitIndex < total; i++) {
    const nibble = parseInt(hex[i] ?? "0", 16);
    for (let b = 3; b >= 0 && bitIndex < total; b--) {
      row.push(((nibble >> b) & 1) === 1);
      bitIndex++;
      if (row.length === MASK_W) {
        rows.push(row);
        row = [];
      }
    }
  }
  return rows;
}

/**
 * 按经纬度采样陆地（双线性插值）。
 *
 * 插值是为了让海岸线**平滑**：最近邻采样在球面上会显出 2° 的方格，
 * 而逐像素渲染的代价已经付了，插值几乎免费。
 *
 * 返回值 0..1，0.5 以上算陆地。
 */
export function sampleLand(mask: number[], lat: number, lon: number): number {
  // 经度绕回 [-180,180)，纬度夹到 [-90,90]
  let l = ((lon + 180) % 360 + 360) % 360;
  const la = Math.max(-90, Math.min(90, lat));
  const fx = (l / 360) * MASK_W - 0.5;
  const fy = ((90 - la) / 180) * MASK_H - 0.5;

  const x0 = Math.floor(fx);
  const y0 = Math.floor(fy);
  const tx = fx - x0;
  const ty = fy - y0;

  const at = (x: number, y: number) => {
    if (y < 0 || y >= MASK_H) return 0;
    const xx = ((x % MASK_W) + MASK_W) % MASK_W;
    return mask[y * MASK_W + xx] ?? 0;
  };

  const v00 = at(x0, y0);
  const v10 = at(x0 + 1, y0);
  const v01 = at(x0, y0 + 1);
  const v11 = at(x0 + 1, y0 + 1);
  const top = v00 + (v10 - v00) * tx;
  const bot = v01 + (v11 - v01) * tx;
  return top + (bot - top) * ty;
}

/** 把解码结果打成扁平数组（逐像素采样要用一维下标）。 */
export function decodeLandMaskFlat(hex: string): number[] {
  const rows = decodeLandMask(hex);
  const flat: number[] = new Array(MASK_W * MASK_H).fill(0);
  for (let r = 0; r < rows.length; r++) {
    const line = rows[r];
    if (!line) continue;
    for (let c = 0; c < MASK_W; c++) {
      flat[r * MASK_W + c] = line[c] ? 1 : 0;
    }
  }
  return flat;
}
