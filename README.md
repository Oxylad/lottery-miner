# Lottery Miner

Windows tray app (Rust, GPUI + tray-icon) that runs a solo Bitcoin CPU miner
(cpuminer-opt) on this PC and watches/controls a second miner on a home
server over SSH.

## What it does

- Starts N cpuminer-opt instances (default 4 x 4 threads, pinned with
  `--cpu-affinity` 0x000F/0x00F0/0x0F00/0xF000) at idle priority with no
  console window. Stop kills only the processes it started, by handle.
  Why several instances: ckpool does not answer `mining.extranonce.subscribe`
  in time, so each job gives one 2^32 nonce range. Above ~140 MH/s per
  instance that range runs out before the next job and the miner idles
  ("Nonce range exhausted, extranonce not subscribed."). The UI warns when
  that line appears or an instance goes over 140 MH/s.
- Reads each instance's stats API (`-b 127.0.0.1:4048..`, command `summary`;
  HS/KHS is measured speed, DIFF is the network difficulty) and its stdout log.
- Home server: runs Windows OpenSSH `ssh.exe -o BatchMode=yes
  -o ConnectTimeout=10` against the host in Settings (`user@host` or a Host
  from `~/.ssh/config`, optional port and key). Reads `systemctl`, the
  journal, `sensors` and RAPL energy (two reads 10 s apart). Control is
  limited to `sudo -n systemctl start|stop|restart cpuminer`.
- Pool stats from `https://eusolo.ckpool.org/users/<address>` (404 until the
  first accepted share is normal) and network numbers from mempool.space.
- Odds: blocks per year = hashrate x 31,557,600 / (difficulty x 2^32);
  prize = (3.125 BTC + average fees) x 0.98 x EUR price.
- Closing the window hides it to the tray. Tray menu: Open, Start/Stop CPU
  mining, oxyhome Start/Stop, Quit.

Settings live in `%APPDATA%\lottery-miner\settings.json` and are edited in
the window.

## Build

Needs Rust (rustup, stable MSVC toolchain) and Visual Studio Build Tools
with the C++ workload and a Windows 10/11 SDK.

```
cargo test
cargo build --release
```

The binary is `target\release\lottery-miner.exe`. `--hidden` starts it in
the tray (used by the "Start app at login" Run key).

If the project folder path is very long, the MSVC linker can fail with
LNK1104; set `CARGO_TARGET_DIR` to a short path.

Live miner test (starts the real miner for about a minute):

```
cargo test -- --ignored live_ --nocapture
```

## Idle mining

Like Salad: turn on "Mine when this PC is idle", pick the delay on the slider
(1 min to 2 h) and whether to use the CPU, the GPU or both. After that long
without keyboard or mouse input the app starts those miners, and stops them
on the next input. Miners you started yourself are never stopped by it.
While idle mining runs the PC is kept from sleeping (toggle). Gamepad input
does not count as activity (Windows GetLastInputInfo).

## Logs tab

Everything in one place, newest first: each CPU instance, the GPU miner,
app events (starts, stops, idle mode, server commands, SSH errors) and the
home server journal (read with `journalctl -o short-unix`, no duplicates).
Filter by source, Copy to the clipboard, Clear. Kept in memory only (last
5000 lines).

## GPU miner (gpu-miner)

`gpu-miner/` is a small SHA256d GPU miner written for this app (wgpu compute
shader, DirectX 12 or Vulkan, any GPU vendor). The app starts it like
cpuminer: idle priority, no console, stopped by process handle, stats over
the same `summary` API (default port 4060), pool worker `desktopgpu`.

- It rolls extranonce2 itself, so it never hits the ckpool nonce-range stall.
- Every start runs a self-test that must find the real nonce of block 125552
  on the GPU before it mines.
- RX 7700 XT: about 2.0 GH/s on DX12 at intensity 26, 1.9 GH/s at the default
  25 (17 ms per GPU batch). Vulkan is about half as fast with this kernel.
  Lower the intensity in the app if games or video stutter.

```
gpu-miner.exe --list-devices
gpu-miner.exe --benchmark --seconds 10
gpu-miner.exe -o stratum+tcp://eusolo.ckpool.org:3333 -u <address>.desktopgpu -p x -b 127.0.0.1:4060
```

`cargo build --release --workspace` puts `gpu-miner.exe` next to
`lottery-miner.exe`, where the app looks for it by default.

Note: Cargo.lock pins `gpu-allocator`'s `windows` dependency to 0.62 (what
wgpu-hal 30 needs); gpui uses 0.61. If `cargo update` reverts it, the DX12
backend fails to compile with type mismatches in wgpu-hal.
