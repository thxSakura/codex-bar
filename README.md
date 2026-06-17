# Codex Usage Tray

Windows 系统托盘 Codex 用量弹窗初版。

![Codex Usage Tray 截图](docs/screenshot.png)

## 功能

- 托盘常驻，左键点击显示/隐藏弹窗。
- 启动本机 `codex app-server`，通过官方本地协议读取账号、5 小时、7 天、Codex Spark 和 credits 信息。
- 从本地 `~/.codex/sessions` 的 JSONL token_count 事件生成近 210 天 heatmap。
- 本地缓存最近一次成功数据，网络失败时显示缓存和错误状态。
- 初版不读取浏览器 cookie，不做桌面组件，不做多 provider。

## 安全边界

- 不读取 `CODEX_HOME/auth.json` 或 `%USERPROFILE%\.codex\auth.json`。
- 不读取、不缓存、不展示 `access_token` 或 `refresh_token`。
- 账号限额由本机 Codex app-server 获取，远端请求由官方 Codex runtime 处理。
- 缓存文件只保存脱敏后的 usage 快照、heatmap 和设置。

## Codex CLI 路径

应用需要能启动：

```powershell
codex app-server
```

官方 app-server 文档中 `stdio` 是默认传输，所以不需要额外传 `--listen stdio://`。如果 PowerShell 里运行这条命令提示 `Access is denied` / `アクセスが拒否されました。`，通常是系统解析到了 WindowsApps 包内的 `codex.exe`，这个路径不适合第三方程序直接执行。请安装一个可执行的 Codex CLI，或设置 `CODEX_BIN` 指向可执行的 Codex runtime，例如：

```powershell
[Environment]::SetEnvironmentVariable("CODEX_BIN", "$env:LOCALAPPDATA\OpenAI\Codex\bin\330bd0cba6496126\codex.exe", "User")
```

`CODEX_BIN` 也可以指向包含 `codex.exe` 的目录。应用会自动扫描 `%LOCALAPPDATA%\OpenAI\Codex\bin\*\codex.exe` 并选择最新 runtime。不要把 `CODEX_BIN` 指向 `C:\Program Files\WindowsApps\OpenAI.Codex_...\codex.exe`。

## 开发

需要 Node.js、Rust、Tauri 依赖和 Windows WebView2。

```powershell
npm install
npm run build
npm run tauri:dev
```

构建安装包：

```powershell
npm run tauri:build
```

## 许可证

Apache License 2.0
