# Changelog

## v0.2 — VT 终端仿真 + ghost-text autosuggest

### 新增

- **完整 VT 终端仿真**：基于 `alacritty_terminal`，支持 ANSI 颜色、光标控制、清屏、alt-screen（`vim` / `htop` / `less` / `tmux` 正常显示）。
- **内联输入 + ghost-text 补全**：删除独立输入框，所有按键直接透传给远端 PTY，远端回_echo 驱动 VT 光标；autosuggest 以光标右侧灰色 ghost-text 呈现。
- **鼠标支持**：crossterm 捕获鼠标事件，按 SGR 协议编码后发给远端（需远端程序启用 mouse mode，如 `vim` / `htop`）。
- **Bracketed Paste**：粘贴内容自动包裹 `\x1b[200~...\x1b[201~`，防止远端 shell 解释特殊字符。
- **OSC 7 cwd 跟踪**：远端 shell 通过 OSC 7 上报当前目录，补全引擎可按目录优先排序。
- **OSC 133 prompt 标记**：支持显式 prompt 标记（starship 等），精确识别用户输入区。
- **启发式 prompt 检测**：无 OSC 133 的 shell（bash/zsh/fish）通过 Enter + 换行后光标位置自动猜测 prompt。
- **五条件门控**：ghost-text 仅在满足全部条件时显示（prompt 已知、非 alt-screen、影子缓冲与 VT 光标长度对齐等），避免在 vim/htop 等场景乱涂。

### 变更

- **Tab 行为**：Tab 不再接受补全，改为直接透传给远端 shell（用于 Tab 补全）；`→` 在光标位于行尾且存在补全时接受 ghost-text。
- **Ctrl-Q 退出**：唯一本地截获的 Ctrl-* 快捷键；其余全部透传给远端。
- **键盘编码**：完整的 xterm 键序列编码（Ctrl/Alt/Shift、方向键 CSI/SS3、F1-F12、Home/End/Insert/Delete/PageUp/PageDown）。

### 移除

- 独立输入框和行模式 ANSI 剥离（`ansi.rs`），改为 raw passthrough。

### 内部

- 新增 `term.rs`：alacritty_terminal 封装 + OSC 7/133 嗅探。
- 新增 `keys.rs`：crossterm KeyEvent → xterm 字节序列编码器 + SGR mouse 编码。
- 新增 `lib.rs`：cli crate 变为 lib+bin 双目标，支持集成测试。
- 新增 `vt_integration.rs`：8 条 CLI 级集成测试（VT 渲染、alt-screen、OSC 往返、ghost-text 门控、启发式 prompt、raw passthrough）。
- 新增 `echo_server.rs` 集成测试：端到端 ANSI 序列断言。

## v0.1 — MVP

- SSH 密码 / 私钥认证（`russh`）。
- PTY shell + line-mode 远端输出。
- Fish 风格灰色补全（按 host 聚合，SQLite 持久化）。
- Tab / → 接受补全。
- Ctrl-C / Ctrl-D / Ctrl-L / Ctrl-Q / Ctrl-U 本地快捷键。
- 终端窗口大小同步。
- 端到端集成测试（in-process echo server）。
