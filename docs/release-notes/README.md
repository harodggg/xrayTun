# `docs/release-notes/`：每个版本的「用户动作」——**单一来源**

## 约定

每个 tag `v<版本>` 必须有同名文件：

```
docs/release-notes/v0.8.35.md    ← tag v0.8.35 的用户动作
docs/release-notes/v0.8.36.md    ← tag v0.8.36 的用户动作
```

`release.yml` 在发布那一步调用：

```bash
python3 scripts/bump-release.py notes --tag "$TAG" --out NOTES.md
```

它把**本文件的内容**放在 Release 正文最前，后面接固定的安装说明模板。
**缺文件 / 空文件 / 既没有 Markdown 标题（`## …`，允许写成 `> ## …`）也没有下面那句显式声明
⇒ 发布步骤直接失败**（宁可发不出去，也不许「本版要重装助手」这种事静默消失）。

确实没有任何必须动作的版本，就**显式**写：

```
本版无需用户额外动作
```

（显式写出来才算「有意为之」；漏写 = 失败。）

## 为什么

v0.8.35 需要「**重新**安装特权助手」（helper 侧改了代码），而 `release.yml` 里那份正文模板
是**静态**的 —— 实测 `v0.8.34` 的 release body 逐字就是模板，只写了通用的「点安装 helper」。
结果那一版的正文只能**发布后手工** `gh release edit` 补，属于「靠人记」。
本目录把「版本特有的用户动作」变成**一处、必须存在、可本地演练**的输入
（演练见 `docs/verification/verify-release-notes-step.sh`）。
