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
