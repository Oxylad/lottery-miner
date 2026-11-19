#!/usr/bin/env python3
"""Lottery Miner exporter for the home server (oxyhome).

Read-only Prometheus exporter: follows the cpuminer and xmrig journals (shares,
jobs, hashrate), asks XMRig's local HTTP API and monerod's local RPC, and reads
CPU temperature and package power. It never starts, stops or changes anything.

Scraped by the OVH Prometheus over Tailscale (job lottery_oxyhome).
Stdlib only. Settings come from the environment (see lm-exporter.service).
"""

import json
import os
import subprocess
import threading
import time
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

LISTEN = os.environ.get("LM_LISTEN", "127.0.0.1:9841")
XMRIG_API = os.environ.get("LM_XMRIG_API", "http://127.0.0.1:4071/2/summary")
MONEROD_RPC = os.environ.get("LM_MONEROD_RPC", "http://127.0.0.1:18081")
UNITS = ("cpuminer", "xmrig", "monerod")
MINER_UNITS = {"cpuminer.service": ("btc", "cpuminer"), "xmrig.service": ("xmr", "xmrig")}
RAPL = "/sys/class/powercap/intel-rapl:0"
VERSION = "1.0.0"


class MinerLog:
    """State from one miner's journal lines (mirrors src/parse.rs in the app)."""

    def __init__(self):
        self.hashrate = None
        self.accepted = 0
        self.submitted = 0
        self.jobs = 0
        self.last_job = None
        self.last_share_diff = None
        self.best_share_diff = None
        self.last_share_ts = None
        self.blocks_found = 0
        self.connected = False
        self.target_diff = None
        self.block = None
        self.stalls = 0

    def feed(self, msg, ts, live):
        line = msg.strip()
        if line.startswith("[") and "]" in line:
            line = line[line.index("]") + 1:].strip()
        # XMRig
        if "speed 10s/60s/15m" in line:
            rest = line.split("speed 10s/60s/15m", 1)[1].split()
            if rest and rest[0] != "n/a":
                self.hashrate = _num(rest[0])
            return
        if " accepted (" in line or " rejected (" in line:
            inner = line[line.index("(") + 1:line.index(")")]
            if "/" in inner:
                a, r = inner.split("/", 1)
                if a.strip().isdigit() and r.strip().isdigit():
                    self.accepted = int(a)
                    self.submitted = int(a) + int(r)
            # The difficulty the pool credits (its target), not the real one;
            # the real best comes from the XMRig API.
            if " accepted (" in line:
                d = _num(_after(line, ") diff "))
                if d is not None:
                    self.last_share_diff = d
                    self.last_share_ts = ts
                    if self.best_share_diff is None or d > self.best_share_diff:
                        self.best_share_diff = d
            return
        if "new job from " in line:
            self._job(ts, live)
            self.connected = True
            self.target_diff = _after(line, "diff ") or self.target_diff
            h = _after(line, "height ")
            self.block = int(h) if h else self.block
            return
        if "use pool " in line:
            self.connected = True
            return
        # cpuminer-opt
        if "Submitted Diff " in line:
            d = _num(line.split("Submitted Diff ", 1)[1].split(",")[0].split()[0])
            if d is not None:
                self.last_share_diff = d
                self.last_share_ts = ts
                if self.best_share_diff is None or d > self.best_share_diff:
                    self.best_share_diff = d
        elif "BLOCK SOLVED" in line:
            if live:
                self.blocks_found += 1
        elif line.startswith("Stratum connection established"):
            self.connected = True
        elif line.startswith("Stratum connection interrupted"):
            self.connected = False
        elif line.startswith("Nonce range exhausted"):
            if live:
                self.stalls += 1
        elif line.startswith("New Stratum Diff"):
            self.target_diff = _num((_after(line, "New Stratum Diff ") or "").rstrip(",")) or self.target_diff
            b = (_after(line, "Block ") or "").rstrip(",")
            self.block = int(b) if b.isdigit() else self.block
            self._job(ts, live)
        elif line.startswith("New Work:") or line.startswith("New Block "):
            b = (_after(line, "Block ") or "").rstrip(",")
            self.block = int(b) if b.isdigit() else self.block
            self._job(ts, live)
        elif line.startswith("Hash rate") and "(" in line:
            self.hashrate = _hashrate(line[line.index("(") + 1:line.index(")")])
        elif line.startswith("Submitted"):
            n = _last_int(line)
            self.submitted = n if n is not None else self.submitted
        elif line.startswith("Accepted"):
            n = _last_int(line)
            self.accepted = n if n is not None else self.accepted

    def _job(self, ts, live):
        if live:
            self.jobs += 1
        self.last_job = ts


def _num(s):
    try:
        return float(s)
    except (TypeError, ValueError):
        return None


def _after(line, key):
    if key not in line:
        return None
    rest = line.split(key, 1)[1].split()
    return rest[0] if rest else None


def _last_int(line):
    for t in reversed(line.split()):
        if t.isdigit():
            return int(t)
    return None


def _hashrate(text):
    t = text.strip()
    if not t.endswith("h/s"):
        return None
    t = t[:-3]
    mult = {"k": 1e3, "K": 1e3, "M": 1e6, "G": 1e9, "T": 1e12}.get(t[-1:], 1.0)
    if t[-1:].isalpha():
        t = t[:-1]
    v = _num(t)
    return v * mult if v is not None else None


LOGS = {u: MinerLog() for u in MINER_UNITS}
LOCK = threading.Lock()
STARTED = time.time()
JOURNAL_OK = [0]


def follow_journal():
    """Primes gauges from recent history (no counting), then follows live lines."""
    units = []
    for u in MINER_UNITS:
        units += ["-u", u]
    cursor = None
    try:
        out = subprocess.run(
            ["journalctl", "-o", "json", "-n", "5000", "--no-pager"] + units,
            capture_output=True, text=True, timeout=60,
        ).stdout
        for raw in out.splitlines():
            cursor = _handle(raw, live=False) or cursor
        JOURNAL_OK[0] = 1
    except Exception:
        pass
    args = ["journalctl", "-o", "json", "-f", "--no-pager"] + units
    args += ["--after-cursor", cursor] if cursor else ["-n", "0"]
    while True:
        try:
            p = subprocess.Popen(args, stdout=subprocess.PIPE, text=True)
            JOURNAL_OK[0] = 1
            for raw in p.stdout:
                c = _handle(raw, live=True)
                if c:
                    args = ["journalctl", "-o", "json", "-f", "--no-pager"] + units + ["--after-cursor", c]
            p.wait()
        except Exception:
            pass
        JOURNAL_OK[0] = 0
        time.sleep(5)


def _handle(raw, live):
    try:
        e = json.loads(raw)
    except ValueError:
        return None
    unit = e.get("_SYSTEMD_UNIT")
    msg = e.get("MESSAGE")
    if unit not in LOGS or not isinstance(msg, str):
        return e.get("__CURSOR")
    ts = int(e.get("__REALTIME_TIMESTAMP", "0")) / 1e6
    with LOCK:
        LOGS[unit].feed(msg, ts, live)
    return e.get("__CURSOR")


def get_json(url, body=None, timeout=4):
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.loads(r.read().decode())


def units_state():
    out = subprocess.run(
        ["systemctl", "show", "--timestamp=unix", "-p", "Id,ActiveState,UnitFileState,NRestarts,ActiveEnterTimestamp"]
        + [u + ".service" for u in UNITS],
        capture_output=True, text=True, timeout=10,
    ).stdout
    res, cur = {}, {}
    for line in out.splitlines() + [""]:
        if not line.strip():
            if cur.get("Id"):
                res[cur["Id"].removesuffix(".service")] = cur
            cur = {}
            continue
        k, _, v = line.partition("=")
        cur[k] = v
    return res


ENERGY = {}


def package_watts():
    try:
        with open(RAPL + "/energy_uj") as f:
            e = int(f.read())
        now = time.monotonic()
        prev = ENERGY.get("last")
        ENERGY["last"] = (now, e)
        if not prev or now - prev[0] < 1:
            return ENERGY.get("watts")
        de = e - prev[1]
        if de < 0:
            with open(RAPL + "/max_energy_range_uj") as f:
                de += int(f.read())
        ENERGY["watts"] = de / 1e6 / (now - prev[0])
        return ENERGY["watts"]
    except OSError:
        return None


def cpu_temp():
    for h in sorted(os.listdir("/sys/class/hwmon")):
        base = "/sys/class/hwmon/" + h
        try:
            if open(base + "/name").read().strip() == "coretemp":
                return int(open(base + "/temp1_input").read()) / 1000.0
        except OSError:
            continue
    return None


class Writer:
    def __init__(self):
        self.fam = {}

    def add(self, name, kind, help_, labels, value):
        if value is None:
            return
        value = float(value)
        if value != value or value in (float("inf"), float("-inf")):
            return
        lab = ",".join('%s="%s"' % (k, str(v).replace("\\", "\\\\").replace('"', '\\"')) for k, v in labels.items())
        f = self.fam.setdefault(name, (kind, help_, []))
        f[2].append("%s%s %r" % (name, "{%s}" % lab if lab else "", value))

    def gauge(self, name, help_, labels, value):
        self.add(name, "gauge", help_, labels, value)

    def counter(self, name, help_, labels, value):
        self.add(name, "counter", help_, labels, value)

    def text(self):
        out = []
        for name, (kind, help_, samples) in self.fam.items():
            out += ["# HELP %s %s" % (name, help_), "# TYPE %s %s" % (name, kind)] + samples
        return "\n".join(out) + "\n"


def collect():
    w = Writer()
    w.gauge("lm_exporter_info", "Exporter version (always 1).", {"version": VERSION}, 1)
    w.gauge("lm_exporter_start_timestamp_seconds", "When the exporter started (unix s).", {}, STARTED)
    w.gauge("lm_source_up", "Last fetch from this data source worked.", {"source": "journal"}, JOURNAL_OK[0])

    try:
        units = units_state()
    except Exception:
        units = {}
    for u in UNITS:
        s = units.get(u, {})
        lab = {"unit": u}
        w.gauge("lm_unit_active", "systemd unit is active (1) or not (0).", lab, 1 if s.get("ActiveState") == "active" else 0)
        w.gauge("lm_unit_enabled", "systemd unit is enabled (1) or not (0).", lab, 1 if s.get("UnitFileState") == "enabled" else 0)
        w.counter("lm_unit_restarts_total", "Automatic restarts systemd counted.", lab, _num(s.get("NRestarts")))
        since = s.get("ActiveEnterTimestamp", "")
        if since.startswith("@") and s.get("ActiveState") == "active":
            w.gauge("lm_unit_active_since_timestamp_seconds", "When the unit became active (unix s).", lab, _num(since[1:]))

    xmrig = None
    if units.get("xmrig", {}).get("ActiveState") == "active":
        try:
            xmrig = get_json(XMRIG_API)
            w.gauge("lm_source_up", "Last fetch from this data source worked.", {"source": "xmrig_api"}, 1)
        except Exception:
            w.gauge("lm_source_up", "Last fetch from this data source worked.", {"source": "xmrig_api"}, 0)

    with LOCK:
        for unit, (coin, miner) in MINER_UNITS.items():
            if units.get(miner, {}).get("ActiveState") != "active":
                continue
            st = LOGS[unit]
            lab = {"coin": coin, "miner": miner, "proc": "0"}
            hashrate, acc, rej, best, target = st.hashrate, st.accepted, st.submitted - st.accepted, st.best_share_diff, st.target_diff
            uptime = None
            if miner == "xmrig" and xmrig:
                tot = [x for x in xmrig.get("hashrate", {}).get("total", []) if x is not None]
                hashrate = tot[0] if tot else hashrate
                r = xmrig.get("results", {})
                acc = r.get("shares_good", acc)
                rej = r.get("shares_total", acc) - acc
                bests = [b for b in r.get("best", []) if b]
                best = max([best or 0] + bests) or None
                target = r.get("diff_current", target)
                uptime = xmrig.get("uptime")
                st.connected = bool(xmrig.get("connection", {}).get("uptime"))
            since = _num(units.get(miner, {}).get("ActiveEnterTimestamp", "@")[1:])
            if uptime is None and since:
                uptime = time.time() - since
            w.gauge("lm_hashrate_hps", "Measured hashrate, H/s.", lab, hashrate or 0)
            w.gauge("lm_miner_connected", "Connected to the pool (1) or not (0).", lab, 1 if st.connected else 0)
            w.gauge("lm_miner_uptime_seconds", "Seconds since this miner process started.", lab, uptime)
            w.counter("lm_shares_total", "Shares since the miner started, by pool verdict.", dict(lab, result="accepted"), acc)
            w.counter("lm_shares_total", "Shares since the miner started, by pool verdict.", dict(lab, result="rejected"), max(rej, 0))
            w.counter("lm_jobs_total", "Jobs (new work) received from the pool.", lab, st.jobs)
            w.gauge("lm_job_last_timestamp_seconds", "When the last job arrived (unix s).", lab, st.last_job)
            w.counter("lm_stalls_total", "Times the miner ran out of nonces.", lab, st.stalls)
            w.counter("lm_blocks_found_total", "Shares at or above network difficulty (blocks).", lab, st.blocks_found)
            w.gauge("lm_share_target_difficulty", "Share difficulty the pool asks for.", lab, _num(target))
            w.gauge("lm_share_last_difficulty", "Difficulty of the latest share (XMRig logs the difficulty the pool credits).", lab, st.last_share_diff)
            w.gauge("lm_share_best_difficulty", "Best share difficulty since the miner started.", lab, best)
            w.gauge("lm_share_last_timestamp_seconds", "When the latest share was submitted (unix s).", lab, st.last_share_ts)
            w.gauge("lm_job_block_height", "Block height of the current job.", lab, st.block)

    w.gauge("lm_cpu_temp_celsius", "CPU package temperature.", {}, cpu_temp())
    w.gauge("lm_cpu_power_watts", "CPU package power (RAPL).", {}, package_watts())

    try:
        info = get_json(MONEROD_RPC + "/get_info")
        w.gauge("lm_source_up", "Last fetch from this data source worked.", {"source": "monerod"}, 1)
        w.gauge("lm_node_info", "Monero node version (always 1).", {"version": info.get("version", "")}, 1)
        w.gauge("lm_node_height", "Monero node height.", {}, info.get("height"))
        w.gauge("lm_node_target_height", "Height the network is at (0 when synced).", {}, max(info.get("target_height", 0), info.get("height", 0)))
        w.gauge("lm_node_synchronized", "Node says it is synchronized.", {}, 1 if info.get("synchronized") else 0)
        w.gauge("lm_node_peers", "Peer connections.", {"direction": "out"}, info.get("outgoing_connections_count"))
        w.gauge("lm_node_peers", "Peer connections.", {"direction": "in"}, info.get("incoming_connections_count"))
        w.gauge("lm_node_database_bytes", "Blockchain database size.", {}, info.get("database_size"))
        w.gauge("lm_node_txpool_size", "Transactions in the node's pool.", {}, info.get("tx_pool_size"))
        w.gauge("lm_node_difficulty", "Difficulty at the node's tip.", {}, info.get("difficulty"))
        hdr = get_json(MONEROD_RPC + "/json_rpc", {"jsonrpc": "2.0", "id": "0", "method": "get_last_block_header"})
        h = hdr.get("result", {}).get("block_header", {})
        w.gauge("lm_node_last_block_timestamp_seconds", "Time of the node's tip block (unix s).", {}, h.get("timestamp"))
        w.gauge("lm_node_last_block_reward_coins", "Reward of the node's tip block, XMR.", {}, (h.get("reward") or 0) / 1e12 or None)
    except Exception:
        w.gauge("lm_source_up", "Last fetch from this data source worked.", {"source": "monerod"}, 0)
    return w.text()


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path.split("?")[0] != "/metrics":
            self.send_error(404)
            return
        try:
            body = collect().encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/plain; version=0.0.4; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        except Exception as e:
            self.send_error(500, str(e)[:200])

    def log_message(self, *args):
        pass


def main():
    threading.Thread(target=follow_journal, daemon=True).start()
    host, port = LISTEN.rsplit(":", 1)
    ThreadingHTTPServer((host, int(port)), Handler).serve_forever()


if __name__ == "__main__":
    main()
