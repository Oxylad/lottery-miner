//! gpu-miner: solo SHA256d GPU miner (wgpu: Vulkan or DX12) for stratum v1
//! pools, with a cpuminer-compatible command line and stats API so the
//! Lottery Miner app can drive it like cpuminer-opt.

mod gpu;
mod miner;
mod sha256;
mod work;

use std::time::{Duration, Instant};

use gpu::Gpu;
use work::{GpuWork, gpu_work, hex_decode};

/// Block 125552, the Bitcoin wiki's worked example; nonce 2504433986.
const TEST_HEADER: &str = "0100000081cd02ab7e569e8bcd9317e2fe99f2de44d49ab2b8851ba4a308000000000000e320b6c2fffc8d750423db8b1eb942ae710e951ed797f7affc8892b0f1fc122bc7f5d74df2b9441a42a14695";
const TEST_NONCE: u32 = 2504433986;

fn test_work() -> GpuWork {
    let h: [u8; 80] = hex_decode(TEST_HEADER).unwrap().try_into().unwrap();
    gpu_work(&h)
}

/// Finds the real nonce of block 125552 on the GPU and checks every
/// candidate against the CPU implementation.
fn selftest(gpu: &Gpu, threads: u32, iter: u32) -> Result<(), String> {
    let w = test_work();
    let span = threads.div_ceil(gpu::WORKGROUP) * gpu::WORKGROUP * iter;
    let base = TEST_NONCE.wrapping_sub(span / 3);
    let found = gpu.search(&w, base, threads, iter)?;
    if !found.contains(&TEST_NONCE) {
        return Err(format!("GPU did not find the known nonce; candidates {found:?}"));
    }
    for n in &found {
        let st = sha256::hash_from_midstate(&w.midstate, &w.tail, *n);
        if st[7] != 0 {
            return Err(format!("GPU candidate {n} does not check out on the CPU"));
        }
    }
    println!("selftest ok: found nonce {TEST_NONCE} ({} candidate(s) in {span} nonces)", found.len());
    Ok(())
}

fn benchmark(gpu: &Gpu, threads: u32, iter: u32, secs: u64) -> Result<f64, String> {
    let w = test_work();
    let per = (threads.div_ceil(gpu::WORKGROUP) * gpu::WORKGROUP) as u64 * iter as u64;
    let start = Instant::now();
    let mut done = 0u64;
    let mut base = 0u32;
    while start.elapsed() < Duration::from_secs(secs) {
        gpu.search(&w, base, threads, iter)?;
        base = base.wrapping_add(per as u32);
        done += per;
    }
    let hs = done as f64 / start.elapsed().as_secs_f64();
    println!(
        "benchmark: {:.2} MH/s ({} nonces per dispatch, {:.1} ms per dispatch)",
        hs / 1e6,
        per,
        per as f64 / hs * 1000.0
    );
    Ok(hs)
}

fn arg_value(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1).cloned())
}

const USAGE: &str = "gpu-miner: solo SHA256d GPU miner (wgpu)

Mining (cpuminer-style options):
  -o URL              stratum+tcp://host:port
  -u USER             usually <address>.<worker>
  -p PASS             default x
  -b HOST:PORT        stats API (cpuminer 'summary' format)
  --intensity N       2^N nonces per GPU dispatch, 16-30 (default 25)
  --iter N            nonces per GPU thread (default 64)
  --device TEXT       GPU name filter (default: first discrete GPU)
  --backend dx12|vulkan  (default: dx12, then vulkan)
  --suggest-diff D    ask the pool for share difficulty D (testing)
  --verbose           log the previous block hash of each job
Other:
  --list-devices  --selftest  --benchmark [--seconds N]
  -a, --no-color, -t, --cpu-affinity are accepted and ignored.";

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("{USAGE}");
        return;
    }
    if args.iter().any(|a| a == "--list-devices") {
        for a in gpu::list_adapters() {
            println!("{a}");
        }
        return;
    }
    let device = arg_value(&args, "--device");
    let backend = arg_value(&args, "--backend");
    let intensity: u32 = arg_value(&args, "--intensity")
        .and_then(|v| v.parse().ok())
        .unwrap_or(25)
        .clamp(16, 30);
    let iter: u32 = arg_value(&args, "--iter").and_then(|v| v.parse().ok()).unwrap_or(64);
    // 2^intensity nonces per dispatch.
    let threads = ((1u64 << intensity) / iter as u64).max(gpu::WORKGROUP as u64) as u32;

    let gpu = match Gpu::new(device.as_deref(), backend.as_deref()) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("GPU init failed: {e}");
            std::process::exit(2);
        }
    };
    println!("GPU: {} ({})", gpu.name, gpu.backend);
    if let Err(e) = selftest(&gpu, threads, iter) {
        eprintln!("selftest FAILED: {e}");
        std::process::exit(3);
    }
    if args.iter().any(|a| a == "--selftest") {
        return;
    }
    if args.iter().any(|a| a == "--benchmark") {
        let secs = arg_value(&args, "--seconds").and_then(|v| v.parse().ok()).unwrap_or(10);
        if let Err(e) = benchmark(&gpu, threads, iter, secs) {
            eprintln!("benchmark failed: {e}");
            std::process::exit(4);
        }
        return;
    }
    let Some(url) = arg_value(&args, "-o") else {
        eprintln!("missing -o URL\n\n{USAGE}");
        std::process::exit(1);
    };
    let Some(user) = arg_value(&args, "-u") else {
        eprintln!("missing -u USER\n\n{USAGE}");
        std::process::exit(1);
    };
    let cfg = miner::Config {
        url,
        user,
        pass: arg_value(&args, "-p").unwrap_or_else(|| "x".into()),
        api: arg_value(&args, "-b"),
        suggest_diff: arg_value(&args, "--suggest-diff").and_then(|v| v.parse().ok()),
        verbose: args.iter().any(|a| a == "--verbose"),
        threads,
        iter,
    };
    if let Err(e) = miner::run(gpu, cfg) {
        miner::log(&format!("fatal: {e}"));
        std::process::exit(5);
    }
}
