#!/usr/bin/env python3
"""Generates the Lottery Miner Grafana dashboard (lottery-miner.json).

Data: job lottery_miner (the desktop app via the Pushgateway, instance=desktop)
and job lottery_oxyhome (the exporter on oxyhome, instance=oxyhome).
Run: python3 make_dashboard.py > lottery-miner.json
"""

import json

DS = {"type": "prometheus", "uid": "PBFA97CFB590B2093"}
BTC, XMR = "#f7931a", "#f26822"
# Pushgateway keeps the last push if the app dies without quitting: hide a PC
# whose last push is older than 2 minutes (oxyhome has no such series, kept).
STALE = " unless on(instance) (time() - lm_last_push_timestamp_seconds > 120)"
TWO32 = "4294967296"


def fresh(sel):
    return "(%s%s)" % (sel, STALE)


BTC_HR = "sum(%s)" % fresh('lm_hashrate_hps{coin="btc"}')
XMR_HR = "sum(%s)" % fresh('lm_hashrate_hps{coin="xmr"}')
BTC_DIFF = 'scalar(max(lm_network_difficulty{coin="btc"}))'
# Expected Bitcoin blocks per second at our hashrate.
BTC_BLOCKS_PER_SEC = "(%s / (%s * %s))" % (BTC_HR, BTC_DIFF, TWO32)
XMR_PER_DAY = '(%s / scalar(max(lm_network_hashrate_hps{coin="xmr"})) * 720 * scalar(max(lm_block_reward_coins{coin="xmr"})))' % XMR_HR

_id = [0]
_y = [0]


def nid():
    _id[0] += 1
    return _id[0]


def target(expr, legend="", instant=False, fmt=None, ref="A"):
    t = {"datasource": DS, "expr": expr, "refId": ref, "legendFormat": legend}
    if instant:
        t["instant"] = True
        t["range"] = False
    if fmt:
        t["format"] = fmt
    return t


def panel(kind, title, targets, x, w, h, unit=None, desc=None, **extra):
    p = {
        "type": kind,
        "title": title,
        "id": nid(),
        "datasource": DS,
        "gridPos": {"x": x, "y": _y[0], "w": w, "h": h},
        "targets": targets,
        "fieldConfig": {"defaults": {}, "overrides": []},
        "options": {},
    }
    if unit:
        p["fieldConfig"]["defaults"]["unit"] = unit
    if desc:
        p["description"] = desc
    for k, v in extra.items():
        p[k] = v
    return p


def stat(title, expr, x, w=4, h=4, unit=None, desc=None, color=None, decimals=None, mappings=None, thresholds=None, legend=""):
    p = panel("stat", title, [target(expr, legend, instant=True)], x, w, h, unit, desc)
    d = p["fieldConfig"]["defaults"]
    d["color"] = {"mode": "fixed", "fixedColor": color} if color else {"mode": "thresholds"}
    if thresholds:
        d["thresholds"] = {"mode": "absolute", "steps": thresholds}
    if decimals is not None:
        d["decimals"] = decimals
    if mappings:
        d["mappings"] = mappings
    p["options"] = {
        "reduceOptions": {"calcs": ["lastNotNull"], "fields": "", "values": False},
        "colorMode": "value",
        "graphMode": "none",
        "textMode": "auto",
        "justifyMode": "auto",
    }
    return p


def series(title, targets, x, w=12, h=8, unit=None, log=False, stack=False, desc=None, points=False, bars=False, min0=False):
    p = panel("timeseries", title, targets, x, w, h, unit, desc)
    custom = {
        "drawStyle": "bars" if bars else "line",
        "lineWidth": 2,
        "fillOpacity": 60 if bars else (25 if stack else 10),
        "showPoints": "always" if points else "never",
        "pointSize": 5,
        "spanNulls": False,
        "gradientMode": "opacity",
        "stacking": {"mode": "normal" if stack else "none", "group": "A"},
    }
    if log:
        custom["scaleDistribution"] = {"type": "log", "log": 10}
    d = p["fieldConfig"]["defaults"]
    d["custom"] = custom
    d["color"] = {"mode": "palette-classic"}
    if min0:
        d["min"] = 0
    p["options"] = {
        "legend": {"displayMode": "table", "placement": "bottom", "calcs": ["lastNotNull", "max"]},
        "tooltip": {"mode": "multi", "sort": "desc"},
    }
    return p


def color_override(name_regex, color):
    return {
        "matcher": {"id": "byRegexp", "options": name_regex},
        "properties": [{"id": "color", "value": {"mode": "fixed", "fixedColor": color}}],
    }


def row(title):
    r = {"type": "row", "title": title, "id": nid(), "collapsed": False,
         "gridPos": {"x": 0, "y": _y[0], "w": 24, "h": 1}, "panels": []}
    _y[0] += 1
    return r


def advance(h):
    _y[0] += h


UPDOWN = [
    {"type": "value", "options": {"0": {"text": "down", "color": "red", "index": 0},
                                  "1": {"text": "up", "color": "green", "index": 1}}},
]

panels = []

# ---------------------------------------------------------------- overview
panels.append(row("Overview"))
panels += [
    stat("Bitcoin hashrate", BTC_HR, 0, unit="Hs", color=BTC, desc="Desktop CPU (when on Bitcoin) + GPU + oxyhome cpuminer."),
    stat("Monero hashrate", XMR_HR, 4, unit="Hs", color=XMR, desc="XMRig on the desktop and oxyhome."),
    stat("Solo block chance / year", "1 - exp(-%s * 31557600)" % BTC_BLOCKS_PER_SEC, 8, unit="percentunit", decimals=6,
         color="purple", desc="Poisson chance of finding at least one Bitcoin block in a year at the current combined hashrate."),
    stat("Expected time to a block", "1 / %s" % BTC_BLOCKS_PER_SEC, 12, unit="dtdurations", color="purple",
         desc="Average waiting time for one Bitcoin block at the current hashrate (network difficulty x 2^32 / hashrate)."),
    stat("Solo block prize", 'max(lm_solo_prize_eur{coin="btc"})', 16, unit="currencyEUR", color="green",
         desc="Subsidy + average fees, minus the 2% ckpool fee."),
    stat("Est. Monero / day", XMR_PER_DAY, 20, unit="none", decimals=6, color=XMR,
         desc="Your share of the network hashrate x 720 blocks x block reward (before the pool fee)."),
]
advance(4)
panels += [
    stat("BTC price", 'max(lm_price_eur{coin="btc"})', 0, unit="currencyEUR", color=BTC),
    stat("XMR price", 'max(lm_price_eur{coin="xmr"})', 4, unit="currencyEUR", color=XMR),
    stat("Monero EUR / day", '%s * scalar(max(lm_price_eur{coin="xmr"}))' % XMR_PER_DAY, 8, unit="currencyEUR", decimals=4, color="green"),
    stat("SupportXMR balance due", 'max(lm_pool_balance_coins{coin="xmr",state="due"})', 12, unit="none", decimals=8, color=XMR,
         desc="Pending at the pool, paid out at the pool's threshold."),
    stat("Best BTC share vs network", 'max(lm_pool_best_share_difficulty{coin="btc",scope="ever",worker="all"}) / %s' % BTC_DIFF,
         16, unit="percentunit", decimals=8, color="purple",
         desc="How close the best share ever (ckpool) came to a block. 100% = a block."),
    stat("Mining components up",
         "sum(%s) + sum(lm_unit_active{unit=~\"cpuminer|xmrig\"})" % fresh('lm_component_up{component=~"cpu_miner|gpu_miner"}'),
         20, color="green", desc="Desktop CPU + GPU miners and the oxyhome miner unit."),
]
advance(4)

# ---------------------------------------------------------------- hashrate
panels.append(row("Hashrate"))
p = series("Bitcoin hashrate by miner",
           [target("sum by (instance, miner) %s" % fresh('lm_hashrate_hps{coin="btc"}'), "{{instance}} {{miner}}")],
           0, unit="Hs", stack=True)
panels.append(p)
p = series("Monero hashrate by miner",
           [target("sum by (instance, miner) %s" % fresh('lm_hashrate_hps{coin="xmr"}'), "{{instance}} {{miner}}")],
           12, unit="Hs", stack=True)
panels.append(p)
advance(8)
panels.append(series("Hashrate the pools see", [
    target('lm_pool_hashrate_hps{coin="btc",window="1m"}', "ckpool {{worker}} (1m)"),
    target('lm_pool_hashrate_hps{coin="xmr"}', "supportxmr {{worker}}", ref="B"),
], 0, unit="Hs", log=True, desc="Share-based estimates from ckpool (per worker) and SupportXMR. Noisy at low share rates."))
panels.append(series("Per process (desktop cpuminer instances, GPU, XMRig)", [
    target("%s" % fresh("lm_hashrate_hps"), "{{instance}} {{miner}} #{{proc}}"),
], 12, unit="Hs", log=True))
advance(8)

# ---------------------------------------------------------------- shares
panels.append(row("Shares"))
p = series("Bitcoin: best vs latest share (log)", [
    target('max by (instance, miner) %s' % fresh('lm_share_best_difficulty{coin="btc"}'), "best {{instance}} {{miner}}"),
    target('max by (instance, miner) %s' % fresh('lm_share_last_difficulty{coin="btc"}'), "latest {{instance}} {{miner}}", ref="B"),
    target('lm_pool_best_share_difficulty{coin="btc",scope="ever"}', "pool best ever {{worker}}", ref="C"),
    target('lm_pool_best_share_difficulty{coin="btc",scope="round"}', "pool best this round {{worker}}", ref="D"),
    target('max(lm_network_difficulty{coin="btc"})', "network difficulty (a block)", ref="E"),
], 0, h=10, log=True, points=True,
    desc="Share difficulty on a log scale. A share at the network difficulty line is a block.")
p["fieldConfig"]["overrides"] = [color_override("network.*", "red")]
panels.append(p)
p = series("Monero: best vs latest share (log)", [
    target('max by (instance, miner) %s' % fresh('lm_share_best_difficulty{coin="xmr"}'), "best {{instance}} {{miner}}"),
    target('max by (instance, miner) %s' % fresh('lm_share_last_difficulty{coin="xmr"}'), "latest {{instance}} {{miner}}", ref="B"),
    target('max by (instance, miner) %s' % fresh('lm_share_target_difficulty{coin="xmr"}'), "pool target {{instance}} {{miner}}", ref="C"),
    target('max(lm_network_difficulty{coin="xmr"})', "network difficulty (a block)", ref="D"),
], 12, h=10, log=True, points=True,
    desc="Best = XMRig's best real share since it started (its API). Latest = the difficulty the pool credited for the "
         "last accepted share (XMRig logs that, not the real one). A share at the network line would be a Monero block.")
p["fieldConfig"]["overrides"] = [color_override("network.*", "red")]
panels.append(p)
advance(10)


def share_stats(coin, color, x):
    best = 'max(%s)' % fresh('lm_share_best_difficulty{coin="%s"}' % coin)
    net = 'scalar(max(lm_network_difficulty{coin="%s"}))' % coin
    name = {"btc": "BTC", "xmr": "XMR"}[coin]
    return [
        stat("Best %s share" % name, best, x, w=3, unit="short", color=color,
             desc="Best share difficulty of the running miners."),
        stat("Best %s share vs network" % name, "%s / %s" % (best, net), x + 3, w=3, unit="percentunit", decimals=8,
             color="purple", desc="100% = a block."),
        stat("%s shares / hour" % name, 'sum(increase(lm_shares_total{coin="%s",result="accepted"}[1h]))' % coin,
             x + 6, w=3, unit="short", decimals=0, color=color, desc="Accepted shares in the last hour, all miners."),
        stat("Last %s share" % name, 'time() - max(lm_share_last_timestamp_seconds{coin="%s"})' % coin, x + 9, w=3,
             unit="dtdurations", color=color, desc="Time since the latest share any miner submitted."),
    ]


panels += share_stats("btc", BTC, 0) + share_stats("xmr", XMR, 12)
advance(4)
panels.append(series("Accepted shares per hour", [
    target('sum by (instance, miner) (increase(lm_shares_total{result="accepted"}[1h]))', "{{instance}} {{miner}}"),
], 0, w=8, bars=True, min0=True))
panels.append(series("Rejected shares per hour", [
    target('sum by (instance, miner) (increase(lm_shares_total{result="rejected"}[1h]))', "{{instance}} {{miner}}"),
], 8, w=8, bars=True, min0=True))
panels.append(panel("table", "Time since last share", [
    target('time() - max by (instance, miner) (lm_share_last_timestamp_seconds)', "", instant=True, fmt="table"),
    target('time() - max by (worker, coin) (lm_pool_last_share_timestamp_seconds)', "", instant=True, fmt="table", ref="B"),
], 16, 8, 8, unit="dtdurations",
    transformations=[{"id": "merge"}, {"id": "organize", "options": {"excludeByName": {"Time": True}}}]))
advance(8)

# ---------------------------------------------------------------- odds & money
panels.append(row("Odds and money"))
panels += [
    stat("BTC block chance / day", "1 - exp(-%s * 86400)" % BTC_BLOCKS_PER_SEC, 0, unit="percentunit", decimals=8, color="purple"),
    stat("BTC block chance / week", "1 - exp(-%s * 604800)" % BTC_BLOCKS_PER_SEC, 4, unit="percentunit", decimals=7, color="purple"),
    stat("BTC block chance / year", "1 - exp(-%s * 31557600)" % BTC_BLOCKS_PER_SEC, 8, unit="percentunit", decimals=6, color="purple"),
    stat("Odds per year (1 in N)", "1 / (%s * 31557600)" % BTC_BLOCKS_PER_SEC, 12, unit="short", color="purple"),
    stat("XMR solo block chance / day", "1 - exp(-%s / scalar(max(lm_network_hashrate_hps{coin=\"xmr\"})) * 720)" % XMR_HR, 16,
         unit="percentunit", decimals=6, color=XMR, desc="If Monero were mined solo instead of on SupportXMR."),
    stat("SupportXMR paid", 'max(lm_pool_balance_coins{coin="xmr",state="paid"})', 20, unit="none", decimals=6, color=XMR),
]
advance(4)
p = series("Expected value per day (EUR)", [
    target("%s * 86400 * scalar(max(lm_solo_prize_eur{coin=\"btc\"}))" % BTC_BLOCKS_PER_SEC, "Bitcoin solo (expected)"),
    target('%s * scalar(max(lm_price_eur{coin="xmr"}))' % XMR_PER_DAY, "Monero pool", ref="B"),
], 0, w=8, unit="currencyEUR")
p["fieldConfig"]["overrides"] = [color_override("Bitcoin.*", BTC), color_override("Monero.*", XMR)]
panels.append(p)
p = series("Prices", [
    target('max(lm_price_eur{coin="btc"})', "BTC"),
    target('max(lm_price_eur{coin="xmr"})', "XMR", ref="B"),
], 8, w=8, unit="currencyEUR", log=True)
p["fieldConfig"]["overrides"] = [color_override("BTC", BTC), color_override("XMR", XMR)]
panels.append(p)
panels.append(series("SupportXMR balance", [
    target('max by (state) (lm_pool_balance_coins{coin="xmr"})', "{{state}}"),
], 16, w=8, unit="none"))
advance(8)

# ---------------------------------------------------------------- network
panels.append(row("Network and blocks"))
panels += [
    stat("BTC height", 'max(lm_block_height{coin="btc"})', 0, w=3, color=BTC, decimals=0),
    stat("BTC last block age", 'time() - max(lm_block_timestamp_seconds{coin="btc"})', 3, w=3, unit="dtdurations", color=BTC),
    stat("BTC last block txs", 'max(lm_block_tx_count{coin="btc"})', 6, w=3, color=BTC),
    stat("BTC last block fees", 'max(lm_block_fees_coins{coin="btc"})', 9, w=3, decimals=5, color=BTC),
    stat("XMR height", 'max(lm_block_height{coin="xmr"})', 12, w=3, color=XMR, decimals=0),
    stat("XMR last block age", 'time() - max(lm_block_timestamp_seconds{coin="xmr"})', 15, w=3, unit="dtdurations", color=XMR),
    stat("XMR block reward", 'max(lm_block_reward_coins{coin="xmr"})', 18, w=3, decimals=4, color=XMR),
    stat("Monero node", 'max(lm_node_synchronized)', 21, w=3,
         mappings=[{"type": "value", "options": {"0": {"text": "syncing", "color": "orange"}, "1": {"text": "synced", "color": "green"}}}]),
]
advance(4)
tbl = panel("table", "Latest Bitcoin block", [
    target('max by (height, pool) (lm_block_info{coin="btc"})', "", instant=True, fmt="table"),
], 0, 8, 5, transformations=[{"id": "organize", "options": {"excludeByName": {"Time": True, "Value": True}}}])
panels.append(tbl)
panels.append(series("Jobs received per hour", [
    target("sum by (instance, miner) (increase(lm_jobs_total[1h]))", "{{instance}} {{miner}}"),
], 8, w=8, h=5, bars=True, min0=True, desc="New work from the pool. Bitcoin pools send a job every ~30 s plus one per block."))
panels.append(panel("table", "Time since last job", [
    target("time() - max by (instance, miner) (lm_job_last_timestamp_seconds)", "", instant=True, fmt="table"),
], 16, 8, 5, unit="dtdurations", transformations=[{"id": "organize", "options": {"excludeByName": {"Time": True}}}]))
advance(5)
panels.append(series("Network difficulty", [
    target('max(lm_network_difficulty{coin="btc"})', "Bitcoin"),
    target('max(lm_network_difficulty{coin="xmr"})', "Monero", ref="B"),
], 0, w=8, log=True))
panels.append(series("Network hashrate", [
    target('max(lm_network_hashrate_hps{coin="btc"})', "Bitcoin"),
    target('max(lm_network_hashrate_hps{coin="xmr"})', "Monero", ref="B"),
], 8, w=8, unit="Hs", log=True))
panels.append(series("Monero node: height vs network", [
    target("max(lm_node_height)", "node"),
    target('max(lm_block_height{coin="xmr"})', "network (moneroblocks)", ref="B"),
], 16, w=8, desc="Pruned monerod on oxyhome."))
advance(8)

# ---------------------------------------------------------------- health
panels.append(row("Health and activity"))
tl = panel("state-timeline", "Components", [
    target('max by (component) %s' % fresh("lm_component_up"), "desktop {{component}}"),
    target("max by (unit) (lm_unit_active)", "oxyhome {{unit}}", ref="B"),
    target("max by (instance, miner) (lm_miner_connected)", "{{instance}} {{miner}} pool link", ref="C"),
    target("max by (instance, source) (lm_source_up)", "{{instance}} source {{source}}", ref="D"),
], 0, 24, 9)
tl["fieldConfig"]["defaults"]["mappings"] = UPDOWN
tl["fieldConfig"]["defaults"]["color"] = {"mode": "thresholds"}
tl["fieldConfig"]["defaults"]["thresholds"] = {"mode": "absolute", "steps": [{"color": "red", "value": None}, {"color": "green", "value": 1}]}
tl["options"] = {"showValue": "never", "mergeValues": True, "rowHeight": 0.8, "alignValue": "left"}
panels.append(tl)
advance(9)
panels += [
    stat("App uptime", "time() - max(%s)" % fresh("lm_app_start_timestamp_seconds"), 0, unit="dtdurations", color="blue"),
    stat("Last push", "time() - max(lm_last_push_timestamp_seconds)", 4, unit="dtdurations",
         thresholds=[{"color": "green", "value": None}, {"color": "orange", "value": 60}, {"color": "red", "value": 120}]),
    {**stat("App version", "max by (version) (lm_app_info)", 8, color="blue", legend="{{version}}"), "options": {"reduceOptions": {"calcs": ["lastNotNull"], "fields": "", "values": False}, "textMode": "name", "colorMode": "value", "graphMode": "none"}},
    stat("Idle mining", "max(%s)" % fresh("lm_idle_mining"), 12,
         mappings=[{"type": "value", "options": {"0": {"text": "no", "color": "text"}, "1": {"text": "running", "color": "green"}}}]),
    stat("oxyhome CPU temp", "max(lm_cpu_temp_celsius)", 16, unit="celsius",
         thresholds=[{"color": "green", "value": None}, {"color": "orange", "value": 75}, {"color": "red", "value": 90}]),
    stat("oxyhome CPU power", "max(lm_cpu_power_watts)", 20, unit="watt", color="yellow"),
]
advance(4)
panels.append(series("Desktop idle time", [
    target("max(%s)" % fresh("lm_idle_seconds"), "seconds without input"),
    target("max(lm_idle_minutes) * 60", "idle mining threshold", ref="B"),
], 0, w=8, unit="s"))
panels.append(series("oxyhome temperature and power", [
    target("max(lm_cpu_temp_celsius)", "CPU temp (C)"),
    target("max(lm_cpu_power_watts)", "package power (W)", ref="B"),
], 8, w=8))
panels.append(series("Restarts, stalls and reconnects (per hour)", [
    target("sum by (unit) (increase(lm_unit_restarts_total[1h]))", "oxyhome {{unit}} restarts"),
    target("sum by (instance, miner) (increase(lm_stalls_total[1h]))", "{{instance}} {{miner}} stalls", ref="B"),
    target("sum by (instance, miner) (increase(lm_stratum_resets_total[1h]))", "{{instance}} {{miner}} reconnects", ref="C"),
    target("sum by (instance, miner) (increase(lm_blocks_found_total[1h]))", "{{instance}} {{miner}} BLOCKS", ref="D"),
], 16, w=8, bars=True, min0=True))
advance(8)

dashboard = {
    "uid": "lottery-miner",
    "title": "Lottery Miner",
    "tags": ["mining", "lottery-miner"],
    "editable": True,
    "graphTooltip": 1,
    "refresh": "30s",
    "schemaVersion": 39,
    "time": {"from": "now-24h", "to": "now"},
    "timezone": "utc",
    "annotations": {"list": []},
    "templating": {"list": []},
    "panels": panels,
}

print(json.dumps(dashboard, indent=1))
