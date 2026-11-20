#!/usr/bin/env python3
"""Solo Monero stratum for Lottery Miner: a tiny ckpool-style server in front
of the local monerod.

XMRig connects to it like to a pool. Every miner gets its own block template
(monerod's get_block_template with a unique extra_nonce) and an easy share
target that adjusts so each miner finds a share about every SHARE_SECS. Shares
only measure luck (best share, share rate). A share whose hash beats the
network difficulty is a block: it is sent to monerod, and the whole reward goes
to WALLET.

Only your own miners should connect: share hashes are not re-checked with
RandomX here (monerod checks any block before accepting it). Bind it to the
LAN, never to a public address.

Stdlib only. Settings come from the environment (see xmr-solo-stratum.service).
"""

import asyncio
import json
import logging
import os
import struct
import time
import urllib.request

WALLET = os.environ.get("SOLO_WALLET", "")
RPC = os.environ.get("SOLO_RPC", "http://127.0.0.1:18081")
BIND = os.environ.get("SOLO_BIND", "0.0.0.0")
PORT = int(os.environ.get("SOLO_PORT", "3340"))
START_DIFF = int(os.environ.get("SOLO_START_DIFF", "20000"))
MIN_DIFF = int(os.environ.get("SOLO_MIN_DIFF", "1000"))
SHARE_SECS = float(os.environ.get("SOLO_SHARE_SECS", "30"))
# New transactions in the template now and then, even without a new block.
REFRESH_SECS = float(os.environ.get("SOLO_REFRESH_SECS", "60"))

log = logging.getLogger("solo")


def rpc(method, params=None):
    body = json.dumps({"jsonrpc": "2.0", "id": 0, "method": method, "params": params or {}}).encode()
    req = urllib.request.Request(RPC + "/json_rpc", body, {"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=10) as r:
        reply = json.load(r)
    if reply.get("error"):
        raise RuntimeError(f"{method}: {reply['error'].get('message')}")
    return reply["result"]


def chain_tip():
    req = urllib.request.Request(RPC + "/get_height", b"{}", {"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=10) as r:
        reply = json.load(r)
    return reply["height"], reply["hash"]


def varint_len(buf, pos):
    n = 0
    while buf[pos + n] & 0x80:
        n += 1
    return n + 1


def nonce_offset(blob):
    """Block header: major varint, minor varint, timestamp varint, prev id (32), nonce (4)."""
    pos = 0
    for _ in range(3):
        pos += varint_len(blob, pos)
    return pos + 32


def target_hex(diff):
    """64-bit little-endian target XMRig compares against the hash's top 8 bytes."""
    return struct.pack("<Q", min((1 << 64) - 1, ((1 << 64) - 1) // max(1, diff))).hex()


def hash_difficulty(result_hex):
    v = int.from_bytes(bytes.fromhex(result_hex), "little")
    return ((1 << 256) - 1) // max(1, v)


class Stats:
    def __init__(self):
        self.shares = 0
        self.rejected = 0
        self.blocks = 0
        self.best = 0


STATS = Stats()


class Miner:
    counter = 0

    def __init__(self, server, reader, writer):
        Miner.counter += 1
        self.server = server
        self.reader = reader
        self.writer = writer
        self.session = f"{Miner.counter:08x}{os.urandom(4).hex()}"
        self.worker = "?"
        self.diff = START_DIFF
        self.jobs = {}  # job_id -> (template, diff)
        self.job_seq = 0
        self.template = None
        self.template_seq = 0
        self.seen = set()
        self.window_start = time.monotonic()
        self.window_shares = 0
        self.peer = writer.get_extra_info("peername")

    async def send(self, obj):
        self.writer.write((json.dumps(obj) + "\n").encode())
        await self.writer.drain()

    async def fresh_template(self):
        self.template_seq += 1
        # Unique per miner and per template, so no two jobs ever share work.
        extra = f"{self.session}{self.template_seq:08x}"
        t = await asyncio.to_thread(rpc, "get_block_template", {"wallet_address": WALLET, "extra_nonce": extra})
        t["blob_bytes"] = bytes.fromhex(t["blocktemplate_blob"])
        self.template = t

    def make_job(self):
        self.job_seq += 1
        job_id = f"{self.session[:8]}{self.job_seq:x}"
        t = self.template
        self.jobs[job_id] = (t, self.diff)
        # Keep only a few recent jobs; anything older is stale anyway.
        for old in list(self.jobs)[:-8]:
            del self.jobs[old]
        return {
            "blob": t["blockhashing_blob"],
            "job_id": job_id,
            "target": target_hex(self.diff),
            "algo": "rx/0",
            "height": t["height"],
            "seed_hash": t["seed_hash"],
            "id": self.session,
        }

    async def push_job(self, new_template):
        if new_template:
            await self.fresh_template()
        await self.send({"jsonrpc": "2.0", "method": "job", "params": self.make_job()})

    def retarget(self):
        """Aim for one share every SHARE_SECS. Returns True if the target changed."""
        elapsed = time.monotonic() - self.window_start
        if elapsed < 2 * SHARE_SECS and self.window_shares < 8:
            return False
        rate = max(self.window_shares, 0.5) / elapsed
        want = int(self.diff * rate * SHARE_SECS)
        want = max(MIN_DIFF, min(self.diff * 4, max(self.diff // 4, want)))
        self.window_start = time.monotonic()
        self.window_shares = 0
        if abs(want - self.diff) > self.diff * 0.2:
            log.info("%s difficulty %d -> %d", self.worker, self.diff, want)
            self.diff = want
            return True
        return False

    async def handle(self, msg):
        method = msg.get("method")
        mid = msg.get("id")
        p = msg.get("params") or {}
        if method == "login":
            self.worker = str(p.get("rigid") or p.get("pass") or "?")[:32]
            await self.fresh_template()
            job = self.make_job()
            log.info("login %s from %s, start difficulty %d", self.worker, self.peer, self.diff)
            await self.send({"id": mid, "jsonrpc": "2.0", "error": None, "result": {
                "id": self.session, "job": job, "extensions": ["algo", "keepalive"], "status": "OK"}})
        elif method == "submit":
            await self.submit(mid, p)
        elif method == "keepalived":
            await self.send({"id": mid, "jsonrpc": "2.0", "error": None, "result": {"status": "KEEPALIVED"}})
        else:
            await self.send({"id": mid, "jsonrpc": "2.0", "error": {"code": -1, "message": "Unsupported method"}})

    async def reject(self, mid, why):
        STATS.rejected += 1
        log.info("%s share rejected: %s", self.worker, why)
        await self.send({"id": mid, "jsonrpc": "2.0", "error": {"code": -1, "message": why}})

    async def submit(self, mid, p):
        job = self.jobs.get(p.get("job_id"))
        nonce = str(p.get("nonce", ""))
        result = str(p.get("result", ""))
        if job is None:
            return await self.reject(mid, "Unknown or old job")
        t, diff = job
        if t["height"] != self.server.height:
            return await self.reject(mid, "Block expired")
        if len(nonce) != 8 or len(result) != 64:
            return await self.reject(mid, "Malformed share")
        try:
            got = hash_difficulty(result)
            nonce_b = bytes.fromhex(nonce)
        except ValueError:
            return await self.reject(mid, "Malformed share")
        key = (p.get("job_id"), nonce)
        if key in self.seen:
            return await self.reject(mid, "Duplicate share")
        self.seen.add(key)
        if len(self.seen) > 10000:
            self.seen.clear()
        if got < diff:
            return await self.reject(mid, "Low difficulty share")

        STATS.shares += 1
        STATS.best = max(STATS.best, got)
        self.window_shares += 1
        net = int(t["difficulty"])
        log.info("%s share diff %d (target %d, network %d, %.2e short)", self.worker, got, diff, net, net / got)
        if got >= net:
            blob = bytearray(t["blob_bytes"])
            off = nonce_offset(blob)
            blob[off:off + 4] = nonce_b
            try:
                res = await asyncio.to_thread(rpc, "submit_block", [blob.hex()])
                STATS.blocks += 1
                log.warning("BLOCK SOLVED at height %d by %s, share diff %d: %s", t["height"], self.worker, got, res)
            except Exception as e:  # noqa: BLE001
                log.error("BLOCK CANDIDATE at height %d by %s rejected by monerod: %s", t["height"], self.worker, e)
        await self.send({"id": mid, "jsonrpc": "2.0", "error": None, "result": {"status": "OK"}})
        if self.retarget():
            await self.push_job(new_template=False)

    async def run(self):
        try:
            while True:
                line = await asyncio.wait_for(self.reader.readline(), timeout=600)
                if not line:
                    break
                if len(line) > 16384:
                    break
                try:
                    msg = json.loads(line)
                except json.JSONDecodeError:
                    break
                await self.handle(msg)
        except (asyncio.TimeoutError, ConnectionError, RuntimeError, OSError) as e:
            log.info("%s disconnected: %s", self.worker, e)
        finally:
            self.server.miners.discard(self)
            self.writer.close()


class Server:
    def __init__(self):
        self.miners = set()
        self.height = 0
        self.tip = ""

    async def on_connect(self, reader, writer):
        m = Miner(self, reader, writer)
        self.miners.add(m)
        await m.run()

    async def watch_chain(self):
        last_refresh = time.monotonic()
        last_stats = time.monotonic()
        while True:
            try:
                height, tip = await asyncio.to_thread(chain_tip)
                new_block = tip != self.tip
                if new_block or time.monotonic() - last_refresh >= REFRESH_SECS:
                    if new_block and self.tip:
                        log.info("new block at height %d", height)
                    self.height, self.tip = height, tip
                    last_refresh = time.monotonic()
                    for m in list(self.miners):
                        if m.template is None:
                            continue
                        try:
                            m.retarget()
                            await m.push_job(new_template=True)
                        except Exception as e:  # noqa: BLE001
                            log.warning("%s: job push failed: %s", m.worker, e)
                if time.monotonic() - last_stats >= 600:
                    last_stats = time.monotonic()
                    log.info("stats: %d miners, %d shares, %d rejected, best %d, blocks %d",
                             len(self.miners), STATS.shares, STATS.rejected, STATS.best, STATS.blocks)
            except Exception as e:  # noqa: BLE001
                log.warning("node: %s", e)
            await asyncio.sleep(1)

    async def main(self):
        self.height, self.tip = await asyncio.to_thread(chain_tip)
        srv = await asyncio.start_server(self.on_connect, BIND, PORT, limit=32768)
        log.info("listening on %s:%d, node %s at height %d", BIND, PORT, RPC, self.height)
        async with srv:
            await asyncio.gather(srv.serve_forever(), self.watch_chain())


if __name__ == "__main__":
    logging.basicConfig(level=logging.INFO, format="%(levelname)s %(message)s")
    if not (len(WALLET) == 95 and WALLET.startswith("4")):
        raise SystemExit("SOLO_WALLET must be the wallet's main address (95 chars, starts with 4)")
    asyncio.run(Server().main())
