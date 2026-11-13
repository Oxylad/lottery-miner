//! Solo-mining odds and money math. All pure functions.

/// 2^32, hashes per nonce range in one job.
pub const TWO_POW_32: f64 = 4_294_967_296.0;
/// Seconds in a Julian year, used for the yearly odds.
pub const SECS_PER_YEAR: f64 = 31_557_600.0;
pub const SECS_PER_DAY: f64 = 86_400.0;
/// Block subsidy in BTC (post-2024 halving).
pub const SUBSIDY_BTC: f64 = 3.125;
/// ckpool's solo fee.
pub const POOL_FEE: f64 = 0.02;
/// Shares are sent at this difficulty by ckpool solo.
pub const POOL_SHARE_DIFF: f64 = 10_000.0;

/// Expected hashes needed to find one block at this difficulty.
pub fn attempts_per_block(difficulty: f64) -> f64 {
    difficulty * TWO_POW_32
}

/// Expected blocks per day at this hashrate (H/s).
pub fn blocks_per_day(hashrate: f64, difficulty: f64) -> f64 {
    if difficulty <= 0.0 {
        return 0.0;
    }
    hashrate * SECS_PER_DAY / attempts_per_block(difficulty)
}

/// Expected blocks per year at this hashrate (H/s).
pub fn blocks_per_year(hashrate: f64, difficulty: f64) -> f64 {
    if difficulty <= 0.0 {
        return 0.0;
    }
    hashrate * SECS_PER_YEAR / attempts_per_block(difficulty)
}

/// "1 in N" form: N = attempts / (hashes per year). None when hashrate is zero.
pub fn one_in_per_year(hashrate: f64, difficulty: f64) -> Option<f64> {
    let per_year = blocks_per_year(hashrate, difficulty);
    if per_year <= 0.0 {
        None
    } else {
        Some(1.0 / per_year)
    }
}

/// Expected prize in EUR for one block, after the pool fee.
/// `avg_fees_sats` is the average fees per block in satoshis.
pub fn prize_eur(avg_fees_sats: f64, eur_per_btc: f64) -> f64 {
    (SUBSIDY_BTC + avg_fees_sats / 1e8) * (1.0 - POOL_FEE) * eur_per_btc
}

/// Seconds between accepted pool shares at this hashrate (H/s).
pub fn seconds_per_pool_share(hashrate: f64) -> Option<f64> {
    if hashrate <= 0.0 {
        None
    } else {
        Some(POOL_SHARE_DIFF * TWO_POW_32 / hashrate)
    }
}

/// How many times the best share falls short of network difficulty.
pub fn times_short(network_diff: f64, best_share: f64) -> Option<f64> {
    if best_share <= 0.0 {
        None
    } else {
        Some(network_diff / best_share)
    }
}

/// Human readable "1 in N" text.
pub fn format_one_in(n: f64) -> String {
    if !n.is_finite() {
        return "never at this speed".to_string();
    }
    if n >= 1e9 {
        format!("1 in {:.2} billion", n / 1e9)
    } else if n >= 1e6 {
        format!("1 in {:.1} million", n / 1e6)
    } else if n >= 1e3 {
        format!("1 in {:.1} thousand", n / 1e3)
    } else {
        format!("1 in {:.1}", n)
    }
}

/// Formats a hashrate with a unit (H/s input).
pub fn format_hashrate(hs: f64) -> String {
    let (v, unit) = if hs >= 1e12 {
        (hs / 1e12, "TH/s")
    } else if hs >= 1e9 {
        (hs / 1e9, "GH/s")
    } else if hs >= 1e6 {
        (hs / 1e6, "MH/s")
    } else if hs >= 1e3 {
        (hs / 1e3, "kH/s")
    } else {
        (hs, "H/s")
    };
    format!("{v:.2} {unit}")
}

#[cfg(test)]
mod tests {
    use super::*;

    // Reference point from the brief (2026-11-02).
    const DIFF: f64 = 1.4836e14;
    const HR: f64 = 69.4e6;

    #[test]
    fn yearly_odds_match_reference() {
        let n = one_in_per_year(HR, DIFF).unwrap();
        // about 291 million
        assert!((n / 1e6 - 291.0).abs() < 2.0, "got {n}");
        assert_eq!(format_one_in(n), "1 in 290.9 million");
    }

    #[test]
    fn prize_matches_reference() {
        // no fees, 1 EUR per BTC: subsidy after the 2% fee
        assert!((prize_eur(0.0, 1.0) - 3.125 * 0.98).abs() < 1e-9);
        // fees of 500000 sats = 0.005 BTC add to the subsidy
        let p = prize_eur(500_000.0, 100_000.0);
        assert!((p - (3.13 * 0.98 * 100_000.0)).abs() < 1e-6);
    }

    #[test]
    fn pool_share_time() {
        // 286 MH/s -> 10000 * 2^32 / 286e6 s, about 41.7 h (cpuminer says "Share 1d17h")
        let t = seconds_per_pool_share(286e6).unwrap();
        assert!((t / 3600.0 - 41.7).abs() < 0.1, "got {t}");
        assert!(seconds_per_pool_share(0.0).is_none());
    }

    #[test]
    fn times_short_and_zero_guards() {
        assert_eq!(times_short(1000.0, 10.0), Some(100.0));
        assert_eq!(times_short(1000.0, 0.0), None);
        assert_eq!(blocks_per_day(HR, 0.0), 0.0);
        assert!(one_in_per_year(0.0, DIFF).is_none());
    }

    #[test]
    fn hashrate_formatting() {
        assert_eq!(format_hashrate(290e6), "290.00 MH/s");
        assert_eq!(format_hashrate(1.5e9), "1.50 GH/s");
    }
}
