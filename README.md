# npa-remote

NPA remote 插件：Web 远程控制 + 进阶截图/输入/剪贴板能力。可独立使用，也可作为 [NPA](https://github.com/dahaioup/npa) 的 desktop 插件。

许可证：**AGPL-3.0**（屏幕采集/编码基于 [RustDesk](https://github.com/rustdesk/rustdesk) 的 `libs/scrap`）。

## 功能

- **Web 远程控制**（独立模式）：浏览器打开 `http://<节点>:56750`，JPEG/VP8 帧流 + 鼠标/键盘/组合键/滚轮/剪贴板
  - Windows：DXGI 采集 + libvpx VP8 编码（约 1-5 Mbps，浏览器 WebCodecs 解码）
  - macOS/Linux：系统截图回退 + JPEG 流
- **NPA 插件模式**：`npa-remote plugin` 走 stdin/stdout JSON-RPC（`internal/plugin` 协议），为 NPA 提供
  `desktop.screens/screenshot/mouse_*/type/hotkey/info/clipboard_get/clipboard_set`

## 使用

```bash
# 独立模式（Web 远程控制）
npa-remote serve --bind 0.0.0.0 --port 56750

# NPA 插件模式：放到 <npa 数据目录>/plugins/desktop/npa-remote.exe，NPA 自动拉起
npa-remote plugin
```

## 构建（Windows）

```powershell
vcpkg install libvpx:x64-windows-static
$env:VCPKG_ROOT="C:\vcpkg"
cargo build --release
```

产物：`target/release/npa-remote.exe`。GitHub Actions 每次推送自动构建（见 `.github/workflows/build.yml`）。

## 端口规范

官方插件端口段 56750-56799；本插件默认 **56750**（可配）。默认提供局域网访问（`0.0.0.0`，可用 `--bind` 收紧）。
