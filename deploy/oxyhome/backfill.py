#!/usr/bin/env python3
"""One-off: rebuild Lottery Miner history for oxyhome from the systemd journal.

Replays the cpuminer and xmrig journals through the exporter's parser and
writes OpenMetrics samples once a minute (only while a miner was logging),
up to --until (the exporter's start time, so nothing overlaps live data).
Import on the Prometheus host with:
  promtool tsdb create-blocks-from openmetrics lm-backfill.om <data dir>

Run as root (journal access):  python3 backfill.py --until 1795041437 > lm-backfill.om
"""

import argparse
import importlib.util
import json
import subprocess
import sys

spec = importlib.util.spec_from_file_location("exp", "/usr/local/lib/lm-exporter/lm-exporter.py")
exp = importlib.util.module_from_spec(spec)
spec.loader.exec_module(exp)

STEP = 60
GAP = 600  # no log line for 10 min: the miner was not running


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--until", type=float, required=True)
    args = ap.parse_args()

    out = subprocess.run(
        ["journalctl", "-o", "json", "--no-pager", "-u", "cpuminer.service", "-u", "xmrig.service"],
        capture_output=True, text=True, check=True,
    ).stdout
    events = []
    for raw in out.splitlines():
        try:
            e = json.loads(raw)
        except ValueError:
            continue
        unit, msg = e.get("_SYSTEMD_UNIT"), e.get("MESSAGE")
        if unit in exp.MINER_UNITS and isinstance(msg, str):
            ts = int(e["__REALTIME_TIMESTAMP"]) / 1e6
            if ts < args.until:
                events.append((ts, unit, msg))
    events.sort(key=lambda x: x[0])

    series = {}  # (metric, labels tuple) -> list of (ts, value)
    kinds = {}

    def put(metric, kind, labels, ts, value):
        if value is None:
            return
        kinds[metric] = kind
        series.setdefault((metric, labels), []).append((ts, float(value)))

    logs = {u: exp.MinerLog() for u in exp.MINER_UNITS}
    last_line = {u: None for u in exp.MINER_UNITS}
    next_tick = {}
    for ts, unit, msg in events:
        st = logs[unit]
        # A restart (gap) starts a fresh process: counters from zero.
        if last_line[unit] is not None and ts - last_line[unit] > GAP:
            logs[unit] = st = exp.MinerLog()
            next_tick[unit] = None
        st.feed(msg, ts, live=True)
        last_line[unit] = ts
        tick = next_tick.get(unit) or (ts // STEP + 1) * STEP
        while tick <= ts:
            coin, miner = exp.MINER_UNITS[unit]
            lab = (("coin", coin), ("miner", miner), ("proc", "0"))
            put("lm_hashrate_hps", "gauge", lab, tick, st.hashrate)
            put("lm_shares_total", "counter", lab + (("result", "accepted"),), tick, st.accepted)
            put("lm_shares_total", "counter", lab + (("result", "rejected"),), tick, max(st.submitted - st.accepted, 0))
            put("lm_jobs_total", "counter", lab, tick, st.jobs)
            put("lm_job_last_timestamp_seconds", "gauge", lab, tick, st.last_job)
            put("lm_share_target_difficulty", "gauge", lab, tick, exp._num(st.target_diff))
            put("lm_share_last_difficulty", "gauge", lab, tick, st.last_share_diff)
            put("lm_share_best_difficulty", "gauge", lab, tick, st.best_share_diff)
            put("lm_share_last_timestamp_seconds", "gauge", lab, tick, st.last_share_ts)
            put("lm_job_block_height", "gauge", lab, tick, st.block)
            put("lm_miner_connected", "gauge", lab, tick, 1 if st.connected else 0)
            tick += STEP
        next_tick[unit] = tick

    w = sys.stdout.write
    for metric in sorted(kinds):
        # Types are not stored in the TSDB; gauge keeps the names as they are.
        w("# TYPE %s gauge\n" % metric)
        for (m, labels), samples in sorted(series.items()):
            if m != metric:
                continue
            lab = ",".join('%s="%s"' % kv for kv in (("instance", "oxyhome"), ("job", "lottery_oxyhome")) + labels)
            name = metric
            for ts, v in samples:
                w("%s{%s} %r %d\n" % (name, lab, v, int(ts)))
    w("# EOF\n")
    print("series %d, samples %d, events %d" % (len(series), sum(len(s) for s in series.values()), len(events)), file=sys.stderr)


if __name__ == "__main__":
    main()
