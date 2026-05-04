# 开发环境搭建

## 系统要求

| 项目 | 最低版本 | 说明 |
|------|---------|------|
| Rust | 1.85 | 必需。使用 `rustup` 安装 |
| pkg-config | - | 查找系统库（OpenSSL） |
| OpenSSL 开发库 | 1.1+ | `russh` 依赖 |

### 各平台安装

**Ubuntu / Debian**
```bash
sudo apt update
sudo apt install -y build-essential pkg-config libssl-dev
```

**macOS**
```bash
# 已安装 Xcode Command Line Tools 即可
# 如果没有：
xcode-select --install

# 若通过 Homebrew 管理 OpenSSL：
brew install openssl pkg-config
export PKG_CONFIG_PATH="$(brew --prefix openssl)/lib/pkgconfig"
```

**Arch Linux**
```bash
sudo pacman -S base-devel openssl pkg-config
```

**Windows**

推荐用 [MSYS2](https://www.msys2.org/) 或 WSL2：
```bash
# MSYS2 UCRT64
pacman -S mingw-w64-ucrt-x86_64-toolchain mingw-w64-ucrt-x86_64-openssl pkg-config
```

## 安装 Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
rustc --version  # >= 1.85
```

## 克隆项目

```bash
git clone https://github.com/Atituiset/tiny-ssh.git
cd tiny-ssh
```

## 构建

```bash
# Debug 构建（开发用）
cargo build --workspace

# Release 构建
 cargo build --workspace --release
```

产物：
- Debug: `target/debug/tssh`
- Release: `target/release/tssh`

## 运行

```bash
# 密码登录
cargo run --bin tssh -- alice@example.com

# 指定端口
cargo run --bin tssh -- root@10.0.0.5 -p 2222

# 私钥登录
cargo run --bin tssh -- deploy@example.com -i ~/.ssh/id_ed25519

# 非交互（CI/脚本）
TSSH_PW='secret' cargo run --bin tssh -- alice@example.com --password-env TSSH_PW
```

## 测试

```bash
# 全部测试
cargo test --workspace

# 仅单元测试
cargo test --workspace --lib --bins

# 端到端（启动内置 russh echo 服务）
cargo test -p tiny-ssh-core --test echo_server

# CLI 集成测试（VT + OSC + ghost-text）
cargo test -p tiny-ssh-cli --test vt_integration

# 单行输出测试
cargo test --workspace -- --nocapture
```

## 代码检查

```bash
# Clippy
cargo clippy --workspace --all-targets

# Format
cargo fmt -- --check

# 修复格式
cargo fmt
```

## 调试日志

```bash
# 打开 trace 级别日志
RUST_LOG=trace cargo run --bin tssh -- alice@example.com

# 只看 tiny_ssh 模块
RUST_LOG=tiny_ssh_cli=debug,tiny_ssh_core=debug cargo run --bin tssh -- alice@example.com
```

日志默认输出到 **stderr**（TUI 占用 stdout）。

## 目录结构速览

```
tiny-ssh/
├── Cargo.toml              # Workspace 根
├── crates/
│   ├── tiny-ssh-core/      # 跨平台 Core 库
│   │   ├── src/
│   │   │   ├── transport/ssh/   # SSH 传输层
│   │   │   ├── session.rs       # 会话生命周期
│   │   │   ├── history.rs       # SQLite 历史 + Fish 补全
│   │   │   └── suggest.rs       # 建议引擎
│   │   └── tests/
│   │       └── echo_server.rs   # e2e 测试
│   └── tiny-ssh-cli/       # TUI 二进制 + 库
│       ├── src/
│       │   ├── lib.rs           # 库入口（pub mod）
│       │   ├── main.rs          # 二进制入口
│       │   ├── app.rs           # App 状态机
│       │   ├── term.rs          # VT 终端封装
│       │   ├── keys.rs          # 键编码器
│       │   └── ui.rs            # ratatui 渲染
│       └── tests/
│           └── vt_integration.rs # CLI 集成测试
└── docs/
    ├── DESIGN.md             # 架构设计
    └── DEVELOPMENT.md        # 本文件
```

## 常见问题

### `linker cc not found`

安装 C 编译器：
```bash
# Ubuntu/Debian
sudo apt install build-essential

# macOS
xcode-select --install
```

### `could not find openssl via pkg-config`

```bash
# Ubuntu/Debian
sudo apt install libssl-dev pkg-config

# macOS (Homebrew)
brew install openssl pkg-config
export PKG_CONFIG_PATH="$(brew --prefix openssl)/lib/pkgconfig"
```

### `error: package alacritty_terminal requires rust 1.85`

```bash
rustup update
rustc --version  # 确认 >= 1.85
```

### 在 Android Termux 上开发

```bash
pkg install rust openssl pkg-config
```
