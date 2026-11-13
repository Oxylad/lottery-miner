//! Stratum job -> 80-byte block header, merkle root, targets.
//!
//! Byte order follows stratum v1 as pools implement it:
//! - version, nbits, ntime arrive as big-endian hex of a u32; the header
//!   stores them little-endian.
//! - prevhash arrives as the header's 32 bytes with every 4-byte word
//!   byte-swapped.
//! - the merkle root is sha256d(coinbase) folded with the branches as given,
//!   and goes into the header as is.
//! - the nonce is submitted as big-endian hex of the u32 stored little-endian.

use crate::sha256::{midstate, sha256d};

#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    pub job_id: String,
    pub prevhash: String,
    pub coinb1: String,
    pub coinb2: String,
    pub merkle_branch: Vec<String>,
    pub version: String,
    pub nbits: String,
    pub ntime: String,
    pub clean: bool,
}

pub fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    let s = s.trim();
    if s.len() % 2 != 0 {
        return Err(format!("odd hex length: {s}"));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|_| format!("bad hex: {s}")))
        .collect()
}

pub fn hex_encode(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn be_u32(hex: &str) -> Result<u32, String> {
    u32::from_str_radix(hex.trim(), 16).map_err(|_| format!("bad u32 hex: {hex}"))
}

/// Header prevhash bytes from the stratum form (swap each 4-byte word).
pub fn prevhash_from_stratum(hex: &str) -> Result<[u8; 32], String> {
    let b = hex_decode(hex)?;
    if b.len() != 32 {
        return Err("prevhash must be 32 bytes".into());
    }
    let mut out = [0u8; 32];
    for i in 0..8 {
        for j in 0..4 {
            out[i * 4 + j] = b[i * 4 + 3 - j];
        }
    }
    Ok(out)
}

pub fn merkle_root(
    job: &Job,
    extranonce1: &[u8],
    extranonce2: &[u8],
) -> Result<[u8; 32], String> {
    let mut coinbase = hex_decode(&job.coinb1)?;
    coinbase.extend_from_slice(extranonce1);
    coinbase.extend_from_slice(extranonce2);
    coinbase.extend_from_slice(&hex_decode(&job.coinb2)?);
    let mut root = sha256d(&coinbase);
    for branch in &job.merkle_branch {
        let mut cat = root.to_vec();
        cat.extend_from_slice(&hex_decode(branch)?);
        root = sha256d(&cat);
    }
    Ok(root)
}

/// Header with nonce 0, given the merkle root.
pub fn header_with_root(job: &Job, root: &[u8; 32]) -> Result<[u8; 80], String> {
    let mut h = [0u8; 80];
    h[0..4].copy_from_slice(&be_u32(&job.version)?.to_le_bytes());
    h[4..36].copy_from_slice(&prevhash_from_stratum(&job.prevhash)?);
    h[36..68].copy_from_slice(root);
    h[68..72].copy_from_slice(&be_u32(&job.ntime)?.to_le_bytes());
    h[72..76].copy_from_slice(&be_u32(&job.nbits)?.to_le_bytes());
    Ok(h)
}

/// What the GPU needs for one extranonce2: midstate and the 3 known
/// big-endian words of the second block (merkle tail, ntime, nbits).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GpuWork {
    pub midstate: [u32; 8],
    pub tail: [u32; 3],
}

pub fn gpu_work(header: &[u8; 80]) -> GpuWork {
    let mut tail = [0u32; 3];
    for (i, t) in tail.iter_mut().enumerate() {
        *t = u32::from_be_bytes(header[64 + i * 4..68 + i * 4].try_into().unwrap());
    }
    GpuWork {
        midstate: midstate(header),
        tail,
    }
}

/// Hash bytes (as sha256d outputs them) from the final state words.
#[cfg(test)]
pub fn state_to_hash(state: &[u32; 8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, s) in state.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&s.to_be_bytes());
    }
    out
}

/// Difficulty-1 target = 0xffff * 2^208.
const DIFF1: f64 = 65535.0 * 4.113761393303015e62; // 2^208

/// Share difficulty of a hash (hash read as a little-endian 256-bit number).
pub fn hash_difficulty(hash: &[u8; 32]) -> f64 {
    let mut value = 0.0f64;
    for b in hash.iter().rev() {
        value = value * 256.0 + *b as f64;
    }
    if value == 0.0 {
        f64::INFINITY
    } else {
        DIFF1 / value
    }
}

/// Network difficulty from compact nbits (big-endian hex as in stratum).
pub fn nbits_difficulty(nbits_hex: &str) -> Option<f64> {
    let bits = be_u32(nbits_hex).ok()?;
    let exp = (bits >> 24) as i32;
    let mant = (bits & 0x00ff_ffff) as f64;
    if mant == 0.0 {
        return None;
    }
    let target = mant * 256f64.powi(exp - 3);
    Some(DIFF1 / target)
}

/// Display form of a hash (reversed hex, as block explorers show it).
pub fn display_hash(hash: &[u8; 32]) -> String {
    let mut r = *hash;
    r.reverse();
    hex_encode(&r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sha256::hash_from_midstate;

    /// Inverse of prevhash_from_stratum, to build stratum jobs in tests.
    fn prevhash_to_stratum(display: &str) -> String {
        let mut internal = hex_decode(display).unwrap();
        internal.reverse();
        let mut out = vec![0u8; 32];
        for i in 0..8 {
            for j in 0..4 {
                out[i * 4 + j] = internal[i * 4 + 3 - j];
            }
        }
        hex_encode(&out)
    }

    fn reversed(display: &str) -> [u8; 32] {
        let mut b = hex_decode(display).unwrap();
        b.reverse();
        b.try_into().unwrap()
    }

    fn job(prev_display: &str, version: &str, ntime: &str, nbits: &str) -> Job {
        Job {
            job_id: "1".into(),
            prevhash: prevhash_to_stratum(prev_display),
            coinb1: String::new(),
            coinb2: String::new(),
            merkle_branch: vec![],
            version: version.into(),
            nbits: nbits.into(),
            ntime: ntime.into(),
            clean: true,
        }
    }

    #[test]
    fn genesis_header_and_hash() {
        let j = job(
            "0000000000000000000000000000000000000000000000000000000000000000",
            "00000001",
            "495fab29",
            "1d00ffff",
        );
        let root = reversed("4a5e1e4baab89f3a32518a88c31bc87f618f76673e2cc77ab2127b7afdeda33b");
        let mut h = header_with_root(&j, &root).unwrap();
        h[76..80].copy_from_slice(&0x7c2bac1du32.to_le_bytes());
        assert_eq!(
            hex_encode(&h),
            "0100000000000000000000000000000000000000000000000000000000000000000000003ba3edfd7a7b12b27ac72c3e67768f617fc81bc3888a51323a9fb8aa4b1e5e4a29ab5f49ffff001d1dac2b7c"
        );
        let hash = sha256d(&h);
        assert_eq!(
            display_hash(&hash),
            "000000000019d6689c085ae165831e934ff763ae46a2a6c172b3f1b60a8ce26f"
        );
        // The midstate path (what the GPU runs) gives the same hash.
        let w = gpu_work(&h);
        let st = hash_from_midstate(&w.midstate, &w.tail, 0x7c2bac1d);
        assert_eq!(state_to_hash(&st), hash);
        assert_eq!(st[7].swap_bytes(), 0, "top 32 bits of the hash are zero");
    }

    #[test]
    fn block_125552_with_nonzero_prevhash() {
        // The worked example from the Bitcoin wiki "Block hashing algorithm".
        let j = job(
            "00000000000008a3a41b85b8b29ad444def299fee21793cd8b9e567eab02cd81",
            "00000001",
            "4dd7f5c7",
            "1a44b9f2",
        );
        let root = reversed("2b12fcf1b09288fcaff797d71e950e71ae42b91e8bdb2304758dfcffc2b620e3");
        let mut h = header_with_root(&j, &root).unwrap();
        h[76..80].copy_from_slice(&2504433986u32.to_le_bytes());
        let hash = sha256d(&h);
        assert_eq!(
            display_hash(&hash),
            "00000000000000001e8d6829a8a21adc5d38d0a473b144b6765798e61f98bd1d"
        );
        let w = gpu_work(&h);
        assert_eq!(state_to_hash(&hash_from_midstate(&w.midstate, &w.tail, 2504433986)), hash);
        // Difficulty of that block's hash is far above its target difficulty.
        let net = nbits_difficulty("1a44b9f2").unwrap();
        assert!((net - 244112.49).abs() < 1.0, "net diff {net}");
        assert!(hash_difficulty(&hash) > net);
    }

    #[test]
    fn merkle_root_folds_branches() {
        let mut j = job("00".repeat(32).as_str(), "20000000", "00000000", "1d00ffff");
        j.coinb1 = "01".into();
        j.coinb2 = "04".into();
        let en1 = [0x02];
        let en2 = [0x03];
        let base = sha256d(&[1, 2, 3, 4]);
        assert_eq!(merkle_root(&j, &en1, &en2).unwrap(), base);
        let branch = [0xaa; 32];
        j.merkle_branch = vec![hex_encode(&branch)];
        let mut cat = base.to_vec();
        cat.extend_from_slice(&branch);
        assert_eq!(merkle_root(&j, &en1, &en2).unwrap(), sha256d(&cat));
    }

    #[test]
    fn difficulty_basics() {
        assert!((nbits_difficulty("1d00ffff").unwrap() - 1.0).abs() < 1e-9);
        let mut h = [0u8; 32];
        h[26] = 0xff;
        h[27] = 0xff; // value = 0xffff * 2^208 -> diff 1
        assert!((hash_difficulty(&h) - 1.0).abs() < 1e-9);
    }
}
