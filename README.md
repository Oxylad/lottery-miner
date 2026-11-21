<p align="center"><img src="assets/logo.png" alt="Lottery Miner" width="440"></p>

# Lottery Miner

Windows tray app (Rust, GPUI + tray-icon) that runs a solo Bitcoin CPU miner
(cpuminer-opt) on this PC and watches/controls a second miner on a home
server over SSH.

This is a personal project, built for my own machines and setup. It isn't
meant for anyone else to use, so there are no releases, support or
guarantees that it works elsewhere.

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

Only one copy runs at a time (per-user mutex
`Local\LotteryMiner.SingleInstance`). Starting a second copy brings the first
one's window up and exits; with `--hidden` it just exits. Two copies used to
both mine, clash on the miner API ports and take turns overwriting the same
Grafana metrics, which showed up as regular hashrate gaps.

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

Miners are placed in a Windows Job Object with kill-on-close, so if the app
ends without Quit (End task, crash) Windows stops them too; no orphaned
miners. Quit with "Stop this PC's miners on Quit" off clears that first.

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
Every line gets one UTC timestamp (the journal's own time for server lines,
arrival time otherwise) and the view is sorted by it; the miners' own
prefixes are removed because cpuminer writes local time and gpu-miner UTC.
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

## Logo and icons

`assets/icon.svg` (the full icon), `assets/icon-small.svg` (16-32 px variant)
and `assets/logo.svg` (icon plus wordmark, for dark backgrounds) are the
sources. `assets/make-icons.ps1` renders them with headless Edge and packs
`assets/lottery-miner.ico` (16-256 px). `build.rs` embeds it as icon
resource #1 in both exes; the window and the tray icon load it from there.

## Monero (CPUs)

The header switch **CPUs: Bitcoin | Monero** moves the CPU miners on this PC
and on the home server to Monero (RandomX, XMRig) or back. The GPU always
mines Bitcoin; RandomX is a CPU algorithm. Monero goes to a normal pool
(default `pool.supportxmr.com:443`, TLS): same expected value as solo,
steady small payouts. Set your Monero address in the Monero card first.

**Solo mining** (Settings > Monero > Solo mining) skips the pool. Both
XMRigs mine on `xmr-solo-stratum` on the home server
(`deploy/oxyhome/xmr-solo-stratum.py`, stdlib Python, port 3340), a tiny
ckpool-style stratum in front of the local monerod:

- Each miner gets its own block template (`get_block_template` with a
  unique `extra_nonce`) and an easy share target that adjusts so it finds a
  share about every 30 s. Shares are only stats: best share, share rate,
  "times short", like the Bitcoin card.
- A share that beats the network difficulty is a block: the stratum sends it
  to monerod (`submit_block`) and the whole reward goes to `SOLO_WALLET`.
  That must be the wallet's main address (starts with 4); monerod refuses
  subaddresses as the coinbase. The journal logs `BLOCK SOLVED`.
- Share hashes are not re-checked with RandomX, so only your own miners
  should reach it: it listens on IPv4 only (the LAN behind NAT).
- This PC uses `monero.solo_pool` (default `oxyhome.local:3340`), resolved
  to IPv4 at start because `oxyhome.local` also answers with public IPv6
  addresses. The home server's XMRig uses 127.0.0.1:3340.

Install on the server: copy the script to
`/usr/local/lib/xmr-solo-stratum/`, the unit to `/etc/systemd/system/`, put
`SOLO_WALLET=<main address>` in `/etc/default/xmr-solo-stratum`, then
`systemctl enable --now xmr-solo-stratum`.

- This PC: XMRig from `tools\xmrig\xmrig.exe` (or next to the app, or the
  path in Settings), idle priority, no window, stats from its local HTTP API
  (127.0.0.1:4070, `/2/summary`). `tools/` is not in git; get XMRig 6.26.0
  from github.com/xmrig/xmrig and check it against the release's SHA256SUMS.
  Defender flags XMRig (Trojan:Win64/HashvaultMiner.A, threat ID 2147978346);
  it needs an exclusion and an allow, like cpuminer.
- Home server: `xmrig.service` next to `cpuminer.service` (same hardening,
  MemoryMax=3G for the RandomX dataset). Switching runs
  `disable --now` on one unit and `enable --now` on the other, and for Monero
  first writes `/etc/default/xmrig` (pool, validated address, rig name, and
  `XMRIG_OPTS`: `--tls` for a pool, empty for the solo stratum; the unit's
  ExecStart passes `$XMRIG_OPTS` in place of a fixed `--tls`).
  Nothing else on the server is touched.
- The Monero card estimates XMR and EUR per day/year from your hashrate,
  the network hashrate and block reward (moneroblocks.info) and the XMR price
  (CoinGecko).

## Log file and running as administrator

The app writes its log (app events and miner output, not the server's
journal) to `%LOCALAPPDATA%\lottery-miner\app.log`, renamed to
`app.log.old` at 5 MB, including startup steps and any panic.

`deploy\windows\install-admin-task.ps1` (run once, approve the prompt)
creates the scheduled task "Lottery Miner" that starts the app hidden and
elevated at login with no UAC prompt and no time limit, and removes the
normal login entry. Elevated, XMRig applies its MSR tweak (about 10-15% on
RandomX). Leave "Start the app at login" off with the task, or two copies
start.

## Grafana metrics (Prometheus)

The app can push its stats every 15 s to a Prometheus Pushgateway, for the
"Lottery Miner" Grafana dashboard. Turn it on in Settings > Grafana metrics.
URL, user, password and the Cloudflare Access service token live in the
`metrics` section of `%APPDATA%\lottery-miner\settings.json` (never in git).
On Quit the app deletes its group, so the dashboard shows it offline at once;
if the app dies, panels hide data older than 2 minutes.

- What it pushes: hashrate per miner process, accepted/rejected shares, real
  difficulty of the latest and best share, jobs received, blocks found, pool
  stats (ckpool per worker, SupportXMR balance), coin prices, network
  difficulty/hashrate and the latest block of both coins, idle state.
- `deploy/oxyhome/`: read-only exporter for the home server (cpuminer/xmrig
  journals, XMRig API, monerod RPC, CPU temp and power), a systemd unit that
  listens on the Tailscale IP only.
- `deploy/grafana/`: `make_dashboard.py` writes the dashboard JSON (odds are
  computed in PromQL from the combined hashrate), plus the "block found"
  Grafana alert rule, routed to its own Discord contact point
  (`lottery-miner-contact.yaml`, webhook in the server .env as LM_DISCORD_WEBHOOK).
- `deploy/oxyhome/backfill.py`: one-off import of past oxyhome history from the
  journal (hashrate, jobs, shares, best share) via `promtool tsdb create-blocks-from openmetrics`.
- Server side (OVH): Pushgateway container on 127.0.0.1:9091, nginx route
  `/lm-push/metrics/job/lottery_miner/` with basic auth (PUT/POST/DELETE only),
  Prometheus jobs `lottery_miner` (honor_labels) and `lottery_oxyhome`.
