# Claude Code Integration Test — Copy-Paste Commands

Complete command sequence for reproducing the tiny-ssh real-time output test with Claude Code.

## Prerequisites

- tiny-ssh repo cloned and Rust toolchain installed
- `claude` binary available on the target host
- `sshd`, `ssh-keygen`, `ssh` available locally

---

## Step 1: Build tiny-ssh

```bash
cd /root/Projects/tiny-ssh
cargo build --release
```

Binary: `/root/Projects/tiny-ssh/target/release/tssh`

---

## Step 2: Set up local SSH server

### 2.1 Generate auth key pair

```bash
ssh-keygen -t ed25519 -f ~/.ssh/id_ed25519 -N "" -C "tiny-ssh-test"
cat ~/.ssh/id_ed25519.pub >> ~/.ssh/authorized_keys
chmod 600 ~/.ssh/authorized_keys
```

### 2.2 Generate host keys

```bash
mkdir -p /tmp/tiny-ssh-sshd
ssh-keygen -t rsa -f /tmp/tiny-ssh-sshd/ssh_host_rsa_key -N ""
ssh-keygen -t ed25519 -f /tmp/tiny-ssh-sshd/ssh_host_ed25519_key -N ""
```

### 2.3 Write sshd config

```bash
cat > /tmp/tiny-ssh-sshd/sshd_config << 'EOF'
Port 2222
ListenAddress 127.0.0.1
HostKey /tmp/tiny-ssh-sshd/ssh_host_rsa_key
HostKey /tmp/tiny-ssh-sshd/ssh_host_ed25519_key
PermitRootLogin yes
PasswordAuthentication no
PubkeyAuthentication yes
AuthorizedKeysFile /root/.ssh/authorized_keys
EOF
```

### 2.4 Start sshd

```bash
sshd -f /tmp/tiny-ssh-sshd/sshd_config
```

### 2.5 Verify with standard ssh

```bash
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -p 2222 -i ~/.ssh/id_ed25519 root@127.0.0.1 "echo ssh-ok"
```

Expected output: `ssh-ok`

---

## Step 3: Connect with tiny-ssh

```bash
/root/Projects/tiny-ssh/target/release/tssh \
    -i ~/.ssh/id_ed25519 root@127.0.0.1:2222
```

Wait for the bottom status line:

```
[shell open] -> accept · Tab/Ctrl-* remote · Ctrl-Q quit
```

---

## Step 4: Basic smoke test (inside tssh)

Type this inside the tssh session:

```bash
echo hello from inside tssh
```

Expected: `hello from inside tssh` appears in the terminal grid.

---

## Step 5: Real-time output test (inside tssh)

```bash
for i in $(seq 1 10); do echo "tick $i"; sleep 0.5; done
```

**Pass criterion**: `tick 1` appears immediately, then `tick 2` after ~0.5s, and so on. No batching.

---

## Step 6: TUI program tests (inside tssh)

```bash
top -n 1 -b | head -15
```

```bash
watch -n 0.5 date
```

Press `Ctrl-C` to exit `watch`. The key must be forwarded correctly.

---

## Step 7: Claude Code test (inside tssh)

### 7.1 First attempt (expected to fail on Termux/PRoot)

```bash
claude
```

If you see:

```
claude: error while loading shared libraries: /lib/aarch64-linux-gnu/libc.so: invalid ELF header
```

This is caused by `LD_PRELOAD=/data/data/com.termux/files/usr/lib/libtermux-exec-ld-preload.so` injected by the SSH login environment. Proceed to 7.2.

### 7.2 Fix and retry

```bash
unset LD_PRELOAD
claude
```

**Pass criterion**: Claude Code splash screen renders (star/ASCII art pattern), then either connects to API or shows a network error. The TUI graphics must display correctly inside the tiny-ssh VT grid.

---

## Step 8: Cleanup

```bash
pkill -f "sshd -f /tmp/tiny-ssh-sshd"
rm -rf /tmp/tiny-ssh-sshd
```

---

## Quick Reference: One-liner verification

If you already did steps 1-2 and just want to verify real-time output:

```bash
# Connect
/root/Projects/tiny-ssh/target/release/tssh -i ~/.ssh/id_ed25519 root@127.0.0.1:2222

# Inside tssh, run:
for i in $(seq 1 10); do echo "tick $i"; sleep 0.5; done
```

Lines should appear one by one, not all at once.
