<p align="center">
  <img src="./assets/logo.png" width="120" alt="Playmate logo" />
</p>

<h1 align="center">Playmate</h1>

<p align="center">
  局域网双人 FC/NES 模拟器 —— 两台电脑配对，各执 1P / 2P，一起通关。
</p>

<p align="center">
  <a href="https://github.com/zlx2019/playmate/actions/workflows/ci.yml"><img src="https://github.com/zlx2019/playmate/actions/workflows/ci.yml/badge.svg" alt="CI" /></a>
  <a href="https://github.com/zlx2019/playmate/releases"><img src="https://img.shields.io/github/v/release/zlx2019/playmate?include_prereleases" alt="Release" /></a>
  <a href="./LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="License: MIT" /></a>
  <img src="https://img.shields.io/badge/platform-macOS%20%7C%20Windows%20%7C%20Linux-e60012" alt="Platform" />
</p>

<p align="center">
  <a href="./README.md">English</a> · <b>简体中文</b>
</p>

---

Playmate 把同一局域网里的两台电脑变成一台双人红白机：一方创建房间，另一方自动发现、输入 4 位 PIN 码加入。只有主机运行模拟器、只有主机需要 ROM —— 画面和声音实时推流给客机，客机把按键回传。席位随时可换，谁坐 1P 都行。

## ✨ 功能特性

- 🎮 **单机 & 本地双人** —— 一台电脑可单人或双人同屏；键盘自带双人布局，开箱即用。
- 🌐 **局域网联机** —— mDNS 自动发现房间，4 位 PIN 码加入，全程不用输 IP。
- 🖥️ **主机权威** —— 只有主机运行模拟器、只有主机需要 ROM；画面走 XOR 增量 + lz4 压缩推流，局域网内按键延迟约 1~2 帧。
- 🔁 **断线自动重连** —— 网络抖动按指数退避自动重试，重连后直接回到进行中的游戏。
- 🕹️ **手柄即插即用** —— 按接入顺序分配 1P / 2P，无需任何配置。
- ⌨️ **键位自定义** —— 设置页里所有键都能改（支持小键盘与修饰键），保存为 `playmate.toml`。
- 🎨 **红白机风格界面** —— 经典红白配色的暗色复古主题。

## 📥 安装

到 [Releases](https://github.com/zlx2019/playmate/releases) 下载对应平台的包，每个产物旁都附有 `.sha256` 校验和。

| 平台 | 安装包 | 说明 |
|---|---|---|
| macOS（Apple Silicon） | `Playmate-vX.Y.Z-aarch64-apple-darwin.dmg` | 打开后把 Playmate 拖入 Applications |
| macOS（Intel） | `Playmate-vX.Y.Z-x86_64-apple-darwin.dmg` | 打开后把 Playmate 拖入 Applications |
| Windows | `Playmate-vX.Y.Z-x86_64-pc-windows-msvc.zip` | 解压后运行 `Playmate.exe` |
| Linux | `Playmate-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz` | 解压后运行 `Playmate`；包内附 `playmate.desktop` 与图标，可自行集成到桌面环境 |

> 目前的构建尚未签名。macOS 首次打开时请右键选择**打开**（或执行 `xattr -cr /Applications/Playmate.app`）；Windows 的 SmartScreen 也可能弹出确认提示。

## 🕹️ 游戏 ROM

Playmate 不附带任何游戏，请使用自有的合法 `.nes` 拷贝。把文件放入以下任一位置的 `roms/` 文件夹即可，两处会合并扫描、按文件名去重：

- 程序（或 `Playmate.app`）所在目录 —— 便携风格；
- 用户数据目录：macOS `~/Library/Application Support/Playmate`、Windows `%APPDATA%\Playmate`、Linux `~/.config/playmate`。

游戏选择页提供**打开 ROM 文件夹**与**刷新**按钮，不用记路径。联机时只有主机需要 ROM 文件。

## 🎮 默认键位

| FC 按键 | 1P | 2P |
|---|---|---|
| 方向 | W / A / S / D | ↑ / ↓ / ← / → |
| B | J | 小键盘 0 |
| A | K | 小键盘 . |
| Select | 左 Shift | ——（真机 2P 手柄无此键） |
| Start | 回车 | 小键盘回车 |

所有键位可在设置页重新绑定；`Esc` 为保留键（退出游戏返回菜单）。手动编辑配置可参考 [playmate.example.toml](./playmate.example.toml)。

## 🌐 联机四步走

1. 一方进入**局域网联机**页创建房间，PIN 码可自定或自动生成（4 位数字）。
2. 同一局域网内，另一台机器会自动看到该房间 —— 选中并输入 PIN 加入。
3. 房间内双方可自由互换 1P / 2P 席位；主机选择游戏并开始。
4. 任一方退出本局后双方回到房间，可直接再开下一局。

## 🔨 从源码构建

```bash
git clone https://github.com/zlx2019/playmate.git
cd playmate
cargo run --release
```

Rust 版本由 `rust-toolchain.toml` 锁定，`rustup` 会自动安装对应工具链。Linux 需要先装音频与手柄支持的开发头文件：

```bash
sudo apt-get install -y libasound2-dev libudev-dev pkg-config
```

工作区分为三个 crate：`apps/playmate-app`（egui 界面、音频、输入、联机任务）、`deps/playmate-core`（模拟器核心，封装 [tetanes-core](https://github.com/lukexor/tetanes)）、`deps/playmate-net`（网络协议、帧压缩、PIN 握手、mDNS 发现）。代码检查、测试、钩子与发版流程见 [CONTRIBUTING.md](./CONTRIBUTING.md)。

## ❓ 常见问题

**macOS 提示应用已损坏 / 来自身份不明的开发者。**
当前构建尚未公证。右键 → 打开一次即可，或用 `xattr -cr /Applications/Playmate.app` 清除隔离标记。

**macOS 上始终看不到房间。**
macOS 15+ 会在首次启动时申请**本地网络**权限 —— 必须允许，否则 mDNS 发现会静默失败。可在「系统设置 → 隐私与安全性 → 本地网络」中重新开启。

**Windows 上始终看不到房间。**
房间发现依赖防火墙放行 —— 首次运行时 Windows 询问的话，请允许 `Playmate.exe` 在专用网络下通信。

**客机也需要 ROM 文件吗？**
不需要。只有主机加载 ROM；客机只接收画面 / 声音流，并把按键回传。

**联机时该用哪套键位？**
1P 和 2P 两套键位都控制你自己的席位，用哪半边键盘顺手用哪边 —— 对没有小键盘的笔记本尤其友好。

**单人游戏时，按另一套键位也会动我的角色。**
这是游戏本身的行为，不是模拟器 bug：不少 FC 游戏在单人模式下会把两个手柄的输入按位或后一起读取。

## 📄 许可证

[MIT](./LICENSE) —— Playmate 仅为模拟器本体，不包含任何受版权保护的游戏内容。
