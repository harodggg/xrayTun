# 第三方组件

[`LICENSE`](LICENSE) 里的 **MIT 许可只覆盖本仓库的源码**（Rust / TypeScript / 配置与脚本）。

**发布产物（dmg / zip）里随包分发**了以下第三方二进制，它们**各有自己的许可，不受 MIT 覆盖**：

| 组件 | 位置 | 许可 | 说明 |
|---|---|---|---|
| **Xray-core** | `Contents/Resources/xray` | **MPL-2.0** | 以**独立进程**调用，不构成衍生作品；许可证见 [Xray-core 官方仓库](https://github.com/XTLS/Xray-core) |
| `geoip.dat` / `geosite.dat` | `Contents/Resources/` | 随 Xray-core 发布 | 同上 |

## 为什么单独写在这个文件里

我们**刻意不把这段放进 `LICENSE`**：GitHub 的许可证识别只看标准模板，
在 `LICENSE` 里附加段落会让整个仓库被识别为 `Other` 而不是 MIT
（实测过：`gh repo view --json licenseInfo` → `{"key":"other"}`）。
拆成两个文件之后，**`LICENSE` 是纯 MIT 正文**（GitHub 正确识别为 MIT），
而第三方声明仍然完整、且被明确链接。

## 界面与官网的口径

界面与官网提到许可证时**必须如实标注这两层**：

* 本仓库源码是 **MIT**；
* 随包分发的 **Xray-core 是 MPL-2.0**（独立进程、非衍生作品）。

**不要**让人以为整个产物都是 MIT。
