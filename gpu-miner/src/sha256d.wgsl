// SHA256d over an 80-byte block header, nonce search.
// The CPU sends the midstate of the first 64 bytes and the three known
// big-endian words of the second block (merkle tail, ntime, nbits).
// A thread tries ITER consecutive nonces and records every nonce whose
// final hash has its most significant 32 bits zero (difficulty >= 1);
// the CPU checks those against the real share target.

struct Params {
    mid0: vec4<u32>,
    mid1: vec4<u32>,
    tail: vec4<u32>,      // x, y, z = tail words, w = nonce base
    iter: vec4<u32>,      // x = nonces per thread
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read_write> found: array<atomic<u32>>;

const MAX_FOUND: u32 = 63u;

const K = array<u32, 64>(
    0x428a2f98u, 0x71374491u, 0xb5c0fbcfu, 0xe9b5dba5u, 0x3956c25bu, 0x59f111f1u, 0x923f82a4u, 0xab1c5ed5u,
    0xd807aa98u, 0x12835b01u, 0x243185beu, 0x550c7dc3u, 0x72be5d74u, 0x80deb1feu, 0x9bdc06a7u, 0xc19bf174u,
    0xe49b69c1u, 0xefbe4786u, 0x0fc19dc6u, 0x240ca1ccu, 0x2de92c6fu, 0x4a7484aau, 0x5cb0a9dcu, 0x76f988dau,
    0x983e5152u, 0xa831c66du, 0xb00327c8u, 0xbf597fc7u, 0xc6e00bf3u, 0xd5a79147u, 0x06ca6351u, 0x14292967u,
    0x27b70a85u, 0x2e1b2138u, 0x4d2c6dfcu, 0x53380d13u, 0x650a7354u, 0x766a0abbu, 0x81c2c92eu, 0x92722c85u,
    0xa2bfe8a1u, 0xa81a664bu, 0xc24b8b70u, 0xc76c51a3u, 0xd192e819u, 0xd6990624u, 0xf40e3585u, 0x106aa070u,
    0x19a4c116u, 0x1e376c08u, 0x2748774cu, 0x34b0bcb5u, 0x391c0cb3u, 0x4ed8aa4au, 0x5b9cca4fu, 0x682e6ff3u,
    0x748f82eeu, 0x78a5636fu, 0x84c87814u, 0x8cc70208u, 0x90befffau, 0xa4506cebu, 0xbef9a3f7u, 0xc67178f2u,
);

fn rotr(x: u32, n: u32) -> u32 {
    return (x >> n) | (x << (32u - n));
}

fn bswap(x: u32) -> u32 {
    return ((x & 0xffu) << 24u) | ((x & 0xff00u) << 8u) | ((x >> 8u) & 0xff00u) | (x >> 24u);
}

// One SHA-256 compression. `w` holds the 16 message words and is used as a
// rolling schedule window; `s` is the input state, the result is returned.
fn compress(s: array<u32, 8>, w_in: array<u32, 16>) -> array<u32, 8> {
    var w = w_in;
    var a = s[0]; var b = s[1]; var c = s[2]; var d = s[3];
    var e = s[4]; var f = s[5]; var g = s[6]; var h = s[7];
    for (var i = 0u; i < 64u; i = i + 1u) {
        var wi: u32;
        if (i < 16u) {
            wi = w[i];
        } else {
            let w15 = w[(i + 1u) & 15u];
            let w2 = w[(i + 14u) & 15u];
            let s0 = rotr(w15, 7u) ^ rotr(w15, 18u) ^ (w15 >> 3u);
            let s1 = rotr(w2, 17u) ^ rotr(w2, 19u) ^ (w2 >> 10u);
            wi = w[i & 15u] + s0 + w[(i + 9u) & 15u] + s1;
            w[i & 15u] = wi;
        }
        let S1 = rotr(e, 6u) ^ rotr(e, 11u) ^ rotr(e, 25u);
        let ch = (e & f) ^ (~e & g);
        let t1 = h + S1 + ch + K[i] + wi;
        let S0 = rotr(a, 2u) ^ rotr(a, 13u) ^ rotr(a, 22u);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let t2 = S0 + maj;
        h = g; g = f; f = e; e = d + t1;
        d = c; c = b; b = a; a = t1 + t2;
    }
    return array<u32, 8>(s[0] + a, s[1] + b, s[2] + c, s[3] + d, s[4] + e, s[5] + f, s[6] + g, s[7] + h);
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>, @builtin(num_workgroups) nwg: vec3<u32>) {
    let mid = array<u32, 8>(p.mid0.x, p.mid0.y, p.mid0.z, p.mid0.w, p.mid1.x, p.mid1.y, p.mid1.z, p.mid1.w);
    let iv = array<u32, 8>(0x6a09e667u, 0xbb67ae85u, 0x3c6ef372u, 0xa54ff53au,
                           0x510e527fu, 0x9b05688cu, 0x1f83d9abu, 0x5be0cd19u);
    // Flatten a 2D dispatch so more than 65535 workgroups can be used.
    let thread = gid.x + gid.y * nwg.x * 256u;
    let first = p.tail.w + thread * p.iter.x;
    for (var k = 0u; k < p.iter.x; k = k + 1u) {
        let nonce = first + k;
        let h1 = compress(mid, array<u32, 16>(
            p.tail.x, p.tail.y, p.tail.z, bswap(nonce),
            0x80000000u, 0u, 0u, 0u, 0u, 0u, 0u, 0u, 0u, 0u, 0u, 640u));
        let h2 = compress(iv, array<u32, 16>(
            h1[0], h1[1], h1[2], h1[3], h1[4], h1[5], h1[6], h1[7],
            0x80000000u, 0u, 0u, 0u, 0u, 0u, 0u, 256u));
        if (h2[7] == 0u) {
            let slot = atomicAdd(&found[0], 1u);
            if (slot < MAX_FOUND) {
                atomicStore(&found[slot + 1u], nonce);
            }
        }
    }
}
