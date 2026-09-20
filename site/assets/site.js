/* ============================================================================
 * XrayTun 官网的**可选增强**脚本（site/assets/site.js）
 *
 * # 铁律：它不是渲染前提
 *
 * 站点的所有关键内容（首屏、下载链接、未公证安装说明、FAQ）都在 HTML 里。
 * 禁用 JavaScript 时本文件不执行，页面**照样完整可读、可下载**。
 * 所以这里只做两件锦上添花的事，并且**任何异常都必须静默**（catch 掉，
 * 绝不让一段可选脚本把页面搞坏）。
 *
 * 1. 语言切换记忆：访问者点过 English 之后，下次进 `/` 时在导航里给一个
 *    「上次你选了英文」的提示链接（**不是自动跳转** —— 自动跳转会让人无法
 *    停留在中文页，也会让爬虫困惑。切换始终是真实 `<a href>`）。
 * 2. 版本提示（可选）：如果 GitHub API 拿得到，且最新 tag 比页面写死的版本新，
 *    在顶栏加一行「有新版本 vX.Y.Z」。拿不到就什么都不做。
 *
 * 为什么不做「自动检测系统并选择安装路径」：那会让禁用 JS 的用户看到空白，
 * 也违反 VISUAL §5.3.1（三条路径必须都在 HTML 里）。所以路径选择永远是静态 HTML。
 * ========================================================================== */

(function () {
  "use strict";

  /** 页面写死的当前版本（与 HTML 正文/JSON-LD 保持一致）。 */
  var PAGE_VERSION = "0.8.28";
  var REPO_API = "https://api.github.com/repos/harodggg/xrayTun/releases/latest";

  // ---- 1. 语言切换记忆 -----------------------------------------------------
  try {
    var KEY = "xraytun.lang";
    var isEn = document.documentElement.lang === "en";
    // 记录「访问者当前正在看哪种语言」，供另一个语言页面读取
    try {
      localStorage.setItem(KEY, isEn ? "en" : "zh");
    } catch (e) {
      /* 隐私模式下 localStorage 会抛，忽略 */
    }

    var zhLink = document.querySelector('.lang-switch a[lang="zh-Hans"]');
    var enLink = document.querySelector('.lang-switch a[lang="en"]');
    var saved = null;
    try {
      saved = localStorage.getItem(KEY);
    } catch (e) {
      saved = null;
    }
    // 只有「存过语言、且和当前页不同」时才给提示；提示本身是真实链接
    if (saved && ((saved === "en" && !isEn) || (saved === "zh" && isEn))) {
      var target = saved === "en" ? enLink : zhLink;
      var host = document.querySelector(".lang-switch");
      if (target && host && !host.querySelector(".lang-note")) {
        var note = document.createElement("span");
        note.className = "lang-note";
        note.append("（上次你选了 ");
        var a = document.createElement("a");
        a.href = target.getAttribute("href");
        a.textContent = saved === "en" ? "English" : "中文";
        if (saved === "en") a.setAttribute("hreflang", "en");
        note.append(a, "）");
        host.append(note);
      }
    }
  } catch (e) {
    /* 增强失败不影响阅读 */
  }

  // ---- 2. 有新版提示（可选，失败静默） ------------------------------------
  // 只在本页讲的软件**就是 XrayTun** 时才提示新版本。子项目页（例如 /wasm/，讲的是
  // xray-wasm v0.7.0）如果显示「有新版本 vX」——那是拿 XrayTun 的 Release 去说另一个项目，
  // 属于不实陈述。所以那些页面用 <html data-no-update-check> 明确关掉这一段。
  try {
    if (document.documentElement.hasAttribute("data-no-update-check")) return;
    if (!("fetch" in window)) return;
    fetch(REPO_API, { headers: { Accept: "application/vnd.github+json" } })
      .then(function (r) {
        if (!r.ok) throw new Error("http " + r.status);
        return r.json();
      })
      .then(function (rel) {
        var tag = String((rel && rel.tag_name) || "").replace(/^v/, "");
        if (!tag || !/^\d+\.\d+\.\d+$/.test(tag)) return;
        if (cmpVersion(tag, PAGE_VERSION) <= 0) return;
        var bar = document.querySelector(".site-header__inner");
        if (!bar) return;
        var el = document.createElement("a");
        el.className = "meta meta--hint";
        el.href = rel.html_url || "https://github.com/harodggg/xrayTun/releases/latest";
        el.textContent = "有新版本 v" + tag + " ▸";
        bar.append(el);
      })
      .catch(function () {
        /* 网络/限流/离线：静默。页面写死的版本信息仍然是对的。 */
      });
  } catch (e) {
    /* 同上 */
  }

  /** 语义化版本比较：a > b 返回 1。 */
  function cmpVersion(a, b) {
    var pa = a.split(".").map(Number);
    var pb = b.split(".").map(Number);
    for (var i = 0; i < 3; i++) {
      if ((pa[i] || 0) > (pb[i] || 0)) return 1;
      if ((pa[i] || 0) < (pb[i] || 0)) return -1;
    }
    return 0;
  }
})();
