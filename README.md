# tiny-ssh

跨平台 SSH 客户端（雏形），带 Fish 风格的本地历史补全。
长期目标：桌面 / 移动 / Web 三端复用同一个 Rust Core，扩展 DB 客户端与上下文化的智能命令提示。完整设计见 [`docs/DESIGN.md`](docs/DESIGN.md)。

## 项目状态

**v0.1 (MVP)**：CLI/TUI、SSH 终端、本地历史补全。

| 功能 | 状态 |
|------|------|
| SSH 密码 / 私钥认证 | ✅ |
| PTY shell + 远端输出（line-mode，ANSI 剥离） | ✅ |
| Fish 风格灰色补全（按 host 聚合） | ✅ |
| Tab / → 接受补全 | ✅ |
| 命令历史持久化（SQLite） | ✅ |
| Ctrl-C / Ctrl-D / Ctrl-L / Ctrl-Q / Ctrl-U | ✅ |
| 终端窗口大小同步 | ✅ |
| 端到端集成测试（in-process echo server） | ✅ |
| 数据库客户端（MySQL / PostgreSQL） | ⏳ 留接口未实现 |
| 静态知识库 / LLM 建议 | ⏳ 留接口未实现 |
| 完整 VT 终端模拟 | ⏳ v0.2（计划接 `alacritty_terminal`） |
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
│   └── tiny-ssh-cli/         # MVP TUI 二进制（ratatui + crossterm）
│       └── src/{main,app,ui,ansi}.rs
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
| `Tab` / `→`（行尾时） | 接受灰色补全 |
| `Enter` | 提交当前行到远端，并写入历史 |
| `Ctrl-C` | 给远端发 SIGINT |
| `Ctrl-D` | 空行时关闭远端 shell；非空时清空输入 |
| `Ctrl-L` | 仅清空本地滚动区（不影响远端） |
| `Ctrl-U` | 清空当前输入 |
| `Ctrl-Q` | 断开并退出 |

补全只看你在**这个 host 上**用过的命令。新连一台机器时还没有数据，多用几次就开始有效。

## 运行测试

```bash
# 全部
cargo test --workspace

# 单元测试（历史 + ANSI 解析器）
cargo test --workspace --lib --bins

# 端到端（启动一个内置的 russh echo 服务，再用我们的客户端打通它）
cargo test -p tiny-ssh-core --test echo_server
```

## 数据存放位置

历史数据库用 [`directories`](https://docs.rs/directories) 在系统约定目录下打开：

| 平台 | 路径 |
|------|------|
| Linux | `$XDG_DATA_HOME/tiny-ssh/tssh/history.sqlite` 或 `~/.local/share/tiny-ssh/tssh/history.sqlite` |
| macOS | `~/Library/Application Support/io.tinyssh.tssh/history.sqlite` |
| Windows | `%APPDATA%\tinyssh\tssh\data\history.sqlite` |

删除文件即可清空历史。

## 已知限制（v0.1）

- 输出是**行模式**：会剥离 ANSI 转义，颜色和光标控制不会还原。`vim` / `htop` 这类全屏程序在 v0.1 跑起来会很难看，等 v0.2 接 `alacritty_terminal`。
- Host key 校验目前是 `AcceptAny`（**不安全**，仅供开发）。`known_hosts` 校验跟随凭据保险柜在后续版本一起做。
- 远端 PTY 默认开 echo，所以输入框里的内容和滚动区里的远端回显会"重复显示"一次。等支持完整 VT 后会消除。
- 不持久化连接配置。每次都要在命令行写 `user@host`。
- 暂未做 cwd 跟踪，所以补全只按 `host + prefix` 排序，不会按目录细分。

## 路线图

详见 [`docs/DESIGN.md`](docs/DESIGN.md)。下一步优先级：

1. 接 `alacritty_terminal`，让 ANSI / 全屏程序正确渲染
2. `known_hosts` 校验 + `keyring` 凭据保险柜
3. DB 客户端（MySQL / PostgreSQL）+ schema 补全
4. Tauri 2 桌面壳，复用同一个 Core

## 许可

`MIT OR Apache-2.0`。
