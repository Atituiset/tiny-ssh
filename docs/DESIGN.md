# tiny-ssh 设计文档

## 1. 项目目标

打造一款跨平台的轻量 SSH / 数据库客户端，覆盖桌面、移动、Web 三类终端。
对连接到的远端系统提供**上下文感知的命令行智能提示**，让运维与开发都更省心。

支持目标：

- SSH / SFTP 远端 shell（Linux、macOS、BSD、Android Termux、iOS SSH server 等）
- 数据库交互式客户端（MySQL、PostgreSQL，预留扩展）
- 上下文化命令补全（历史、知识库、可选 LLM）

非目标（短期内不做）：

- Telnet、RDP、VNC 等其他远程协议
- 团队协作 / 共享会话 / 录屏审计（企业向功能）
- 服务器侧 agent（保持纯客户端）

## 2. 整体架构

```
┌─────────────────────────────────────────────────┐
│  UI 层（Desktop / Mobile / Web 三端复用）        │
│  - 终端视图（xterm 风格）                        │
│  - DB 查询视图（编辑器 + 结果表格）              │
│  - 会话/凭据管理 UI                              │
└─────────────────────────────────────────────────┘
                       ↕ IPC / FFI
┌─────────────────────────────────────────────────┐
│  Core（Rust 单一静态库 / 二进制）                │
│  ┌──────────────┬──────────────┬──────────────┐ │
│  │ Session Mgr  │ Suggestion   │ Secrets      │ │
│  │ (tabs/复用)  │ Engine       │ Vault        │ │
│  └──────────────┴──────────────┴──────────────┘ │
│  ┌──────────────┬──────────────┬──────────────┐ │
│  │ SSH / SFTP   │ DB Drivers   │ History/     │ │
│  │ (russh)      │ (sqlx)       │ Telemetry    │ │
│  └──────────────┴──────────────┴──────────────┘ │
│  ┌─────────────────────────────────────────────┐│
│  │ Terminal Emulator (alacritty_terminal / VT) ││
│  └─────────────────────────────────────────────┘│
└─────────────────────────────────────────────────┘
```

核心思想：**UI 只是壳，所有协议、状态、智能提示都收敛到 Core**。
桌面 / 手机 / Web 三端共享同一份逻辑，避免行为漂移。

## 3. 技术选型

| 关注点 | 选择 | 理由 |
|--------|------|------|
| Core 语言 | Rust（1.95+） | russh、sqlx、alacritty_terminal、tokio 生态齐全；FFI 友好 |
| 异步运行时 | tokio | russh / sqlx 默认依赖 |
| SSH | `russh` + `russh-keys` | 纯 Rust 实现，无 C 依赖 |
| 终端模拟 | `alacritty_terminal`（Layer 4 引入）| VT 解析成熟，有现成的 Grid 实现 |
| 数据库 | `sqlx`（MySQL / PostgreSQL feature）| async、编译期校验、跨平台 |
| 历史存储 | `rusqlite` + bundled SQLite | 单文件、跨平台、无外部依赖 |
| TUI（MVP） | `ratatui` + `crossterm` | Rust 主流 TUI 栈 |
| 桌面 GUI（后续） | Tauri 2 | 体积小、Rust 后端原生集成 |
| 移动端（后续） | Tauri 2 移动 / Flutter+FFI | Tauri 2 已支持 iOS/Android |
| 凭据存储 | `keyring` | 抽象 macOS Keychain / Windows Credential Manager / Secret Service |

## 4. Crate 划分

```
tiny-ssh/                    # Cargo workspace 根
├── Cargo.toml
├── docs/
│   └── DESIGN.md
└── crates/
    ├── tiny-ssh-core/       # 跨平台 Core 库（无 UI 依赖）
    │   ├── transport/       # SSH / DB 协议
    │   ├── session/         # 会话生命周期
    │   ├── terminal/        # VT 解析 + Grid
    │   ├── history/         # 命令历史 + 补全
    │   ├── suggest/         # 智能提示分层
    │   ├── secrets/         # 凭据存储抽象
    │   └── lib.rs
    └── tiny-ssh-cli/        # MVP TUI 二进制
        ├── ui/              # ratatui 视图
        ├── input/           # 输入处理 + 补全展示
        └── main.rs
```

后续扩展（不阻塞 MVP）：

- `crates/tiny-ssh-tauri/`：桌面 GUI
- `crates/tiny-ssh-ffi/`：给 Flutter / Swift / Kotlin 用的 C-ABI 封装
- `crates/tiny-ssh-llm/`：LLM 后端（OpenAI / Anthropic / Ollama）

## 5. 智能提示分层

按"快 → 慢、便宜 → 贵"四层叠加。

### Layer 1：上下文采集

连接成功后立刻探针：

- SSH：`uname -a`、`cat /etc/os-release`、`echo $SHELL`、`command -v apt dnf pacman brew termux-info`、`pwd`、是否 root
- DB：查 `information_schema`，缓存表 / 列 / 索引 / 视图

结果写入 `HostFingerprint`，供后续所有层使用。

### Layer 2：历史 + Fish 风格补全（本地、零延迟）

- 索引：`(host_fingerprint, cwd, prefix) → commands`
- UI：灰色 inline 补全，Tab 或 → 接受
- 覆盖 70% 日常使用，**MVP 必做**

### Layer 3：静态知识库（本地、毫秒级）

- 内嵌 tldr / man 摘要 + 常用命令参数 schema（JSON）
- 触发：输入 `tar -` 时弹出 `-x -z -v -f` 含义
- DB：根据连接的库联想表名 / 列名（比 LLM 更可靠）

### Layer 4：LLM 兜底（按需触发）

- 触发：用户主动按 `Ctrl-G`，或前几层置信度都很低
- 输入 = `HostFingerprint` + 最近 N 条命令及退出码 + 屏幕末尾若干行 + 当前输入
- 输出 = 命令 + 解释 + 破坏性等级（红 / 黄 / 绿）
- 关键：**敏感字段过滤**，支持本地 Ollama 后端

### 反馈闭环

每条建议记录"来源层 + 是否被采纳"，写回历史库，让模型 / 排序越用越准。

## 6. MVP 范围（v0.1）

只做：**SSH 终端 + Layer 2 历史补全**。

里程碑顺序：

1. **Layer 1：SSH 传输层** — `russh` 客户端封装（密码 / 私钥、PTY、读写、断开）
2. **Layer 2：会话管理** — `Session` 结构、生命周期、事件流
3. **Layer 3：历史 + 补全** — SQLite 后端 + 按上下文返回最佳匹配
4. **Layer 4：TUI 入口** — ratatui 终端视图 + 灰色补全 + Tab 接受
5. **e2e 验证 + README**

**留接口、不实现**的部分：

- DB 客户端 trait（先定义，留 Layer 3 静态知识库的 schema 接口）
- LLM 后端 trait（让 suggest 模块预留 Layer 4 入口）
- 凭据 Vault trait（MVP 用纯文本配置 / 环境变量，trait 留好）

## 7. 关键设计原则

1. **终端语义边界**：智能提示**只在本地输入框层**做，绝不去 hook 远端 readline。用户敲回车前的所有补全是我们的，回车之后归远端 shell。
2. **MVP 不依赖网络服务**：补全靠本地 SQLite 和静态知识库，离线可用。
3. **可观测性**：每条建议记录"来源层 + 是否被采纳"，迭代时才知道哪层在拖后腿。
4. **DB 客户端不是 SSH 子集**：交互模型不同（一次性查询 vs 持续 PTY），UI 上分两类标签页，但共享 Suggestion Engine 与凭据库。
5. **手机端首要场景**是连"手机里的 sshd"（Termux / iOS SSH server），不是反向控制手机本身。

## 8. 风险与开放问题

- **alacritty_terminal 体积**：是否引入还是先做行模式 pass-through？倾向 v0.1 先 pass-through，v0.2 接入。
- **PTY 在不同远端的 TERM 兼容性**：`xterm-256color` 是稳妥默认值。
- **iOS 应用商店审核**：SSH 客户端历史上有过拒审，需要提前研究。
- **LLM 隐私**：默认本地后端，云端要显式开关 + 字段脱敏。

---

文档版本：v0.1（2026-05-03）
