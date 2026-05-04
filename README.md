# tiny-ssh

跨平台 SSH 客户端（雏形），带 Fish 风格的本地历史补全。
长期目标：桌面 / 移动 / Web 三端复用同一个 Rust Core，扩展 DB 客户端与上下文化的智能命令提示。完整设计见 [`docs/DESIGN.md`](docs/DESIGN.md)。

## 项目状态

**v0.2**：完整 VT 终端仿真、内联 ghost-text autosuggest、鼠标/粘贴支持。

| 功能 | 状态 |
|------|------|
| SSH 密码 / 私钥认证 | ✅ |
| PTY shell + 完整 VT 仿真（`alacritty_terminal`） | ✅ |
| Fish 风格灰色补全（按 host + cwd 排序） | ✅ |
| `→` 接受 ghost-text / Tab 透传 | ✅ |
| 命令历史持久化（SQLite） | ✅ |
| Ctrl-Q 本地退出；其余 Ctrl-* 透传远端 | ✅ |
| 鼠标捕获 + SGR 协议 | ✅ |
| Bracketed Paste | ✅ |
| OSC 7 cwd 跟踪 + OSC 133 prompt 标记 | ✅ |
| 终端窗口大小同步 | ✅ |
| 端到端集成测试（echo server + VT 集成） | ✅ |
| 数据库客户端（MySQL / PostgreSQL） | ⏳ 留接口未实现 |
| 静态知识库 / LLM 建议 | ⏳ 留接口未实现 |
| 桌面 GUI（Tauri） / 移动端 | ⏳ |

## 仓库结构

```
tiny-ssh/
├── docs/
│   └── DESIGN.md             # 架构 + 智能提示分层 + 路线图
├── crates/
│   ├── tiny-ssh-core/        # 跨平台 Core 库（SSH / 会话 / 历史 / 补全）
│   │   ├── src/transport/ssh/   Layer 1: SSH 传输
│   │   ├── src/session.rs       Layer 2: 会话生命周期
│   │   ├── src/history.rs       Layer 3: SQLite 历史 + Fish 补全
│   │   ├── src/suggest.rs                建议引擎入口（多层留扩展点）
│   │   └── tests/echo_server.rs e2e: 启动 in-process russh 服务器对打
│   └── tiny-ssh-cli/         # TUI 二进制 + 库（ratatui + crossterm + alacritty_terminal）
│       └── src/{lib,main,app,ui,term,keys}.rs
└── Cargo.toml
```

## 构建

需要 Rust 1.85+：

```bash
cargo build --workspace --release
```

二进制产物在 `target/release/tssh`。

## 快速试用

```bash
# 用法
tssh <user@host>[:port] [-p PORT] [-i KEY_FILE]
       [--password-env VAR] [--passphrase-env VAR]

# 密码登录（默认提示输入）
tssh alice@example.com

# 指定端口
tssh root@10.0.0.5:2222
tssh root@10.0.0.5 -p 2222

# 私钥登录
tssh deploy@example.com -i ~/.ssh/id_ed25519

# 非交互（给 CI / 脚本用）
TSSH_PW='secret' tssh alice@example.com --password-env TSSH_PW
```

如果你在 Android Termux 上跑，本机 sshd 默认监听 `8022`：

```bash
tssh "$(whoami)@127.0.0.1:8022"
```

### TUI 快捷键

| 键 | 行为 |
|----|------|
| `→`（光标在行尾且有补全时） | 接受灰色 ghost-text |
| `Tab` | 透传给远端 shell（用于远端 Tab 补全） |
| `Enter` | 提交当前行到远端，并写入历史 |
| `Ctrl-C` | 给远端发 SIGINT（透传） |
| `Ctrl-D` | 空行时关闭远端 shell；非空时清空输入（透传） |
| `Ctrl-L` | 透传给远端（清屏） |
| `Ctrl-U` | 透传给远端（清空当前行） |
| `Ctrl-Q` | **本地**断开并退出 |
| 鼠标点击 / 滚轮 | 透传给远端（SGR 协议，需远端启用 mouse mode） |

补全只看你在**这个 host 上**用过的命令。新连一台机器时还没有数据，多用几次就开始有效。

## 运行测试

```bash
# 全部
cargo test --workspace

# 单元测试（历史 + VT 终端 + 键编码）
cargo test --workspace --lib --bins

# 端到端（启动一个内置的 russh echo 服务，再用我们的客户端打通它）
cargo test -p tiny-ssh-core --test echo_server

# CLI 集成测试（VT 渲染 + OSC + ghost-text 门控）
cargo test -p tiny-ssh-cli --test vt_integration
```

## 数据存放位置

历史数据库用 [`directories`](https://docs.rs/directories) 在系统约定目录下打开：

| 平台 | 路径 |
|------|------|
| Linux | `$XDG_DATA_HOME/tiny-ssh/tssh/history.sqlite` 或 `~/.local/share/tiny-ssh/tssh/history.sqlite` |
| macOS | `~/Library/Application Support/io.tinyssh.tssh/history.sqlite` |
| Windows | `%APPDATA%\tinyssh\tssh\data\history.sqlite` |

删除文件即可清空历史。

## 已知限制

- 不持久化连接配置。每次都要在命令行写 `user@host`。
- `known_hosts` 自动学习（TOFU）策略已可用，但尚不支持手动编辑或拒绝策略配置。
- 移动端 / 桌面 GUI 尚未开始。

## 路线图

详见 [`docs/DESIGN.md`](docs/DESIGN.md)。下一步优先级：

1. `keyring` 凭据保险柜（密码/私钥免重复输入）
2. DB 客户端（MySQL / PostgreSQL）+ schema 补全
3. Tauri 2 桌面壳，复用同一个 Core
4. WebAssembly 端口（浏览器内 SSH）

## 许可

`MIT OR Apache-2.0`。
