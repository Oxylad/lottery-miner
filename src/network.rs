//! Public data: ckpool user stats and mempool.space network numbers.
//! Turns the JSON into plain structs. Parsing is pure and tested with fixtures.

use serde_json::Value;

use crate::odds;
use crate::remote;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PoolUser {
    /// Share-based hashrates, H/s. Shown greyed out in the UI.
    pub hashrate_1m: Option<f64>,
    pub hashrate_1hr: Option<f64>,
    pub last_share_unix: Option<i64>,
    pub shares: Option<u64>,
    pub best_share: Option<f64>,
    pub best_ever: Option<f64>,
    pub workers: Vec<PoolWorker>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PoolWorker {
    pub name: String,
    pub hashrate_1m: Option<f64>,
    pub hashrate_1hr: Option<f64>,
    pub last_share_unix: Option<i64>,
    pub shares: Option<u64>,
    pub best_share: Option<f64>,
    pub best_ever: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Network {
    pub difficulty: Option<f64>,
    pub eur_per_btc: Option<f64>,
    pub avg_fees_sats: Option<f64>,
    /// Network hashrate, H/s (mempool.space 3-day estimate).
    pub hashrate: Option<f64>,
    pub tip: Option<BtcBlock>,
}

/// Latest Bitcoin block (mempool.space /api/v1/blocks, first entry).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BtcBlock {
    pub height: u64,
    pub timestamp: i64,
    pub tx_count: u64,
    pub size: u64,
    pub difficulty: f64,
    /// Subsidy + fees, in sats.
    pub reward_sats: u64,
    pub fees_sats: u64,
    pub pool: String,
}

pub fn parse_btc_tip(v: &Value) -> Option<BtcBlock> {
    let b = v.as_array()?.first()?;
    let extras = b.get("extras");
    Some(BtcBlock {
        height: b.get("height")?.as_u64()?,
        timestamp: b.get("timestamp").and_then(|x| x.as_i64()).unwrap_or(0),
        tx_count: b.get("tx_count").and_then(|x| x.as_u64()).unwrap_or(0),
        size: b.get("size").and_then(|x| x.as_u64()).unwrap_or(0),
        difficulty: b.get("difficulty").and_then(|x| x.as_f64()).unwrap_or(0.0),
        reward_sats: extras.and_then(|e| e.get("reward")).and_then(|x| x.as_u64()).unwrap_or(0),
        fees_sats: extras.and_then(|e| e.get("totalFees")).and_then(|x| x.as_u64()).unwrap_or(0),
        pool: extras
            .and_then(|e| e.get("pool"))
            .and_then(|p| p.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or("unknown")
            .to_string(),
    })
}

/// SupportXMR stats for one address (atomic units are 1e-12 XMR).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct XmrPool {
    pub hashrate: f64,
    pub last_share_unix: Option<i64>,
    pub total_hashes: f64,
    pub valid_shares: u64,
    pub invalid_shares: u64,
    pub due_xmr: f64,
    pub paid_xmr: f64,
}

pub fn parse_xmr_pool(v: &Value) -> Option<XmrPool> {
    let num = |k: &str| v.get(k).and_then(json_f64);
    Some(XmrPool {
        hashrate: num("hash")?,
        last_share_unix: num("lastHash").map(|t| t as i64).filter(|t| *t > 0),
        total_hashes: num("totalHashes").unwrap_or(0.0),
        valid_shares: num("validShares").unwrap_or(0.0) as u64,
        invalid_shares: num("invalidShares").unwrap_or(0.0) as u64,
        due_xmr: num("amtDue").unwrap_or(0.0) / 1e12,
        paid_xmr: num("amtPaid").unwrap_or(0.0) / 1e12,
    })
}

/// Only for SupportXMR: other pools have other APIs.
pub fn fetch_xmr_pool(pool: &str, address: &str) -> Result<Option<XmrPool>, String> {
    if !pool.contains("supportxmr.com") || address.trim().is_empty() {
        return Ok(None);
    }
    let url = format!("https://www.supportxmr.com/api/miner/{}/stats", address.trim());
    Ok(remote::get_json(&url)?.as_ref().and_then(parse_xmr_pool))
}

/// ckpool reports hashrates as strings like "5.44G" or numbers; both become H/s.
pub fn json_hashrate(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => {
            let s = s.trim();
            if s.is_empty() {
                return None;
            }
            let (num, suffix) = match s.chars().last() {
                Some(c) if c.is_ascii_alphabetic() => (&s[..s.len() - 1], Some(c)),
                _ => (s, None),
            };
            let base: f64 = num.parse().ok()?;
            let mult = match suffix {
                None => 1.0,
                Some('K') | Some('k') => 1e3,
                Some('M') => 1e6,
                Some('G') => 1e9,
                Some('T') => 1e12,
                Some('P') => 1e15,
                Some(_) => return None,
            };
            Some(base * mult)
        }
        _ => None,
    }
}

pub fn parse_pool_user(v: &Value) -> PoolUser {
    let workers = v
        .get("worker")
        .and_then(|w| w.as_array())
        .map(|arr| {
            arr.iter()
                .map(|w| PoolWorker {
                    name: w
                        .get("workername")
                        .and_then(|x| x.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    hashrate_1m: w.get("hashrate1m").and_then(json_hashrate),
                    hashrate_1hr: w.get("hashrate1hr").and_then(json_hashrate),
                    last_share_unix: w.get("lastshare").and_then(|x| x.as_f64()).map(|f| f as i64),
                    shares: w.get("shares").and_then(|x| x.as_u64()),
                    best_share: w.get("bestshare").and_then(|x| x.as_f64()),
                    best_ever: w.get("bestever").and_then(|x| x.as_f64()),
                })
                .collect()
        })
        .unwrap_or_default();
    PoolUser {
        hashrate_1m: v.get("hashrate1m").and_then(json_hashrate),
        hashrate_1hr: v.get("hashrate1hr").and_then(json_hashrate),
        last_share_unix: v.get("lastshare").and_then(|x| x.as_f64()).map(|f| f as i64),
        shares: v.get("shares").and_then(|x| x.as_u64()),
        best_share: v.get("bestshare").and_then(|x| x.as_f64()),
        best_ever: v.get("bestever").and_then(|x| x.as_f64()),
        workers,
    }
}

/// The pool's status page for this address (also what the app reads).
pub fn pool_user_url(address: &str) -> String {
    format!("https://eusolo.ckpool.org/users/{}", address.trim())
}

pub fn fetch_pool_user(address: &str) -> Result<Option<PoolUser>, String> {
    let url = pool_user_url(address);
    Ok(remote::get_json(&url)?.map(|v| parse_pool_user(&v)))
}

/// A JSON number, or a string holding one (mempool.space sends some totals as strings).
fn json_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

pub fn parse_difficulty(v: &Value) -> Option<f64> {
    v.get("currentDifficulty").and_then(json_f64)
}

pub fn parse_eur_price(v: &Value) -> Option<f64> {
    v.get("EUR").and_then(json_f64)
}

/// Average fees per block in sats from reward-stats over the last 144 blocks.
pub fn parse_avg_fees(v: &Value) -> Option<f64> {
    v.get("totalFee").and_then(json_f64).map(|total| total / 144.0)
}

pub fn fetch_network() -> Result<Network, String> {
    let mut net = Network::default();
    if let Some(v) = remote::get_json("https://mempool.space/api/v1/mining/hashrate/3d")? {
        net.difficulty = parse_difficulty(&v);
        net.hashrate = v.get("currentHashrate").and_then(json_f64);
    }
    // The tip is extra; a failure here should not hide the numbers above.
    if let Ok(Some(v)) = remote::get_json("https://mempool.space/api/v1/blocks") {
        net.tip = parse_btc_tip(&v);
    }
    if let Some(v) = remote::get_json("https://mempool.space/api/v1/prices")? {
        net.eur_per_btc = parse_eur_price(&v);
    }
    if let Some(v) = remote::get_json("https://mempool.space/api/v1/mining/reward-stats/144")? {
        net.avg_fees_sats = parse_avg_fees(&v);
    }
    Ok(net)
}

/// Monero network numbers for the earnings estimate.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MoneroNet {
    /// H/s
    pub hashrate: Option<f64>,
    pub difficulty: Option<f64>,
    pub height: Option<u64>,
    /// Last block reward in XMR.
    pub reward_xmr: Option<f64>,
    pub eur_per_xmr: Option<f64>,
    /// Time of the last block (unix s).
    pub last_timestamp: Option<i64>,
}

/// moneroblocks.info /api/get_stats ("last_reward" is in atomic units, 1e-12 XMR).
pub fn parse_monero_stats(v: &Value, net: &mut MoneroNet) {
    net.hashrate = v.get("hashrate").and_then(json_f64);
    net.difficulty = v.get("difficulty").and_then(json_f64);
    net.height = v.get("height").and_then(|x| x.as_u64());
    net.reward_xmr = v.get("last_reward").and_then(json_f64).map(|a| a / 1e12);
    net.last_timestamp = v.get("last_timestamp").and_then(|x| x.as_i64());
}

/// CoinGecko simple price: {"monero":{"eur":454.49}}.
pub fn parse_xmr_eur(v: &Value) -> Option<f64> {
    v.get("monero")?.get("eur").and_then(json_f64)
}

pub fn fetch_monero() -> Result<MoneroNet, String> {
    let mut net = MoneroNet::default();
    if let Some(v) = remote::get_json("https://moneroblocks.info/api/get_stats")? {
        parse_monero_stats(&v, &mut net);
    }
    if let Some(v) = remote::get_json("https://api.coingecko.com/api/v3/simple/price?ids=monero&vs_currencies=eur")? {
        net.eur_per_xmr = parse_xmr_eur(&v);
    }
    Ok(net)
}

/// Expected XMR per day for a hashrate (H/s): your share of the network
/// times 720 blocks a day times the block reward. Same for pool and solo
/// on average (a pool takes its fee from this).
pub fn xmr_per_day(hashrate: f64, net: &MoneroNet) -> Option<f64> {
    let (nh, reward) = (net.hashrate?, net.reward_xmr?);
    if nh <= 0.0 || hashrate <= 0.0 {
        return None;
    }
    Some(hashrate / nh * 720.0 * reward)
}

/// Odds shown in the UI for one hashrate at the network difficulty.
#[derive(Debug, Clone, PartialEq)]
pub struct Odds {
    pub one_in_per_year: Option<f64>,
    pub blocks_per_day: f64,
    pub prize_eur: Option<f64>,
}

pub fn odds_for(hashrate: f64, net: &Network) -> Odds {
    let diff = net.difficulty.unwrap_or(0.0);
    let prize = match (net.avg_fees_sats, net.eur_per_btc) {
        (Some(fees), Some(eur)) => Some(odds::prize_eur(fees, eur)),
        _ => None,
    };
    Odds {
        one_in_per_year: odds::one_in_per_year(hashrate, diff),
        blocks_per_day: odds::blocks_per_day(hashrate, diff),
        prize_eur: prize,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ckpool_user_fixture() {
        let v = json!({
            "hashrate1m": "280M",
            "hashrate5m": "270M",
            "hashrate1hr": "1.2G",
            "lastshare": 1763065200.0,
            "shares": 5234,
            "bestshare": 12345.6,
            "bestever": 987654,
            "authorised": 1763000000,
            "worker": [
                {"workername": "bc1qxxx.desktop", "hashrate1m": "280M", "lastshare": 1763065200.0},
                {"workername": "bc1qxxx.oxyhome", "hashrate1m": "69M", "lastshare": 1763065100.0}
            ]
        });
        let u = parse_pool_user(&v);
        assert_eq!(u.hashrate_1m, Some(280e6));
        assert_eq!(u.hashrate_1hr, Some(1.2e9));
        assert_eq!(u.shares, Some(5234));
        assert_eq!(u.best_share, Some(12345.6));
        assert_eq!(u.workers.len(), 2);
        assert_eq!(u.workers[1].hashrate_1m, Some(69e6));
    }

    #[test]
    fn monero_fixtures_and_earnings() {
        // Captured 2026-11-16.
        let stats: Value = serde_json::from_str(
            r#"{"difficulty":862330176135,"height":3807757,"hashrate":7186084801.125,"total_emission":"18831829362793084111","last_reward":678760166873,"last_timestamp":1794844042}"#,
        )
        .unwrap();
        let price: Value = serde_json::from_str(r#"{"monero":{"eur":454.49},"bitcoin":{"eur":58616}}"#).unwrap();
        let mut net = MoneroNet::default();
        parse_monero_stats(&stats, &mut net);
        net.eur_per_xmr = parse_xmr_eur(&price);
        assert_eq!(net.height, Some(3807757));
        assert!((net.reward_xmr.unwrap() - 0.678760166873).abs() < 1e-12);
        assert_eq!(net.eur_per_xmr, Some(454.49));
        // 20 kH/s: 20e3 / 7.186e9 * 720 * 0.6788 = about 0.00136 XMR a day.
        let d = xmr_per_day(20_000.0, &net).unwrap();
        assert!((d - 0.00136).abs() < 0.00001, "got {d}");
        assert!(xmr_per_day(0.0, &net).is_none());
        assert!(xmr_per_day(1.0, &MoneroNet::default()).is_none());
    }

    #[test]
    fn mempool_fixtures() {
        // Captured 2026-11-13.
        let rewards: Value = serde_json::from_str(
            r#"{"startBlock":975575,"endBlock":975718,"totalReward":"45437446916","totalFee":"437446916","totalTx":"564939"}"#,
        )
        .unwrap();
        let prices: Value = serde_json::from_str(
            r#"{"time":1794607503,"USD":68301,"EUR":62006,"GBP":51789}"#,
        )
        .unwrap();
        let fees = parse_avg_fees(&rewards).unwrap();
        assert!((fees - 437_446_916.0 / 144.0).abs() < 1e-6);
        assert_eq!(parse_eur_price(&prices), Some(62006.0));
        assert_eq!(parse_difficulty(&json!({"currentDifficulty": 1.4836e14})), Some(1.4836e14));
        // (3.125 + 0.0304) x 0.98 x 62006, about 191.7 thousand EUR
        let prize = odds::prize_eur(fees, 62006.0);
        assert!((prize / 1000.0 - 191.7).abs() < 0.2, "got {prize}");
    }

    #[test]
    fn pool_link() {
        assert_eq!(
            pool_user_url(" bc1qxyz "),
            "https://eusolo.ckpool.org/users/bc1qxyz"
        );
    }

    #[test]
    fn hashrate_strings() {
        assert_eq!(json_hashrate(&json!("5.44G")), Some(5.44e9));
        assert_eq!(json_hashrate(&json!("0")), Some(0.0));
        assert_eq!(json_hashrate(&json!(12.5)), Some(12.5));
        assert_eq!(json_hashrate(&json!("")), None);
        assert_eq!(json_hashrate(&json!("3X")), None);
    }

    #[test]
    fn btc_tip_and_supportxmr_fixtures() {
        // Captured 2026-11-18 (trimmed).
        let blocks: Value = serde_json::from_str(
            r#"[{"height":976492,"timestamp":1795038999,"difficulty":134823837323272.7,"tx_count":7655,"size":1648595,"extras":{"reward":312849141,"totalFees":349141,"pool":{"id":111,"name":"Foundry USA","slug":"foundryusa"}}}]"#,
        )
        .unwrap();
        let tip = parse_btc_tip(&blocks).unwrap();
        assert_eq!(tip.height, 976492);
        assert_eq!(tip.pool, "Foundry USA");
        assert_eq!(tip.fees_sats, 349141);
        assert_eq!(tip.reward_sats, 312849141);
        assert!(parse_btc_tip(&json!([])).is_none());

        let stats: Value = serde_json::from_str(
            r#"{"hash":0,"identifier":"global","lastHash":1794848654,"totalHashes":75000,"validShares":1,"invalidShares":0,"expiry":1795039555232,"amtPaid":0,"amtDue":48710,"txnCount":0}"#,
        )
        .unwrap();
        let p = parse_xmr_pool(&stats).unwrap();
        assert_eq!(p.valid_shares, 1);
        assert_eq!(p.last_share_unix, Some(1794848654));
        assert!((p.due_xmr - 4.871e-8).abs() < 1e-15);
        assert_eq!(p.paid_xmr, 0.0);
    }

    #[test]
    fn odds_use_network_numbers() {
        let net = Network {
            difficulty: Some(1.4836e14),
            eur_per_btc: Some(90_000.0),
            avg_fees_sats: Some(300_000.0),
            ..Default::default()
        };
        let o = odds_for(69.4e6, &net);
        let n = o.one_in_per_year.unwrap();
        assert!((n / 1e6 - 291.0).abs() < 2.0);
        assert!(o.prize_eur.unwrap() > 0.0);
        let none = odds_for(69.4e6, &Network::default());
        assert!(none.prize_eur.is_none());
    }
}
