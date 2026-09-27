//! 稳定工具链随机语料循环驱动（任务卡 I2 的降级路径 + CI smoke）。
//!
//! 主路径是 `cargo fuzz run`（libFuzzer 覆盖率引导，nightly）；本驱动在**稳定版
//! 工具链**上以等价语料量循环：xorshift64* PRNG + 种子突变/拼接生成畸形帧与
//! 畸形 JSON，喂给与 libFuzzer 目标相同的 harness 函数，断言不 panic。
//!
//! 用法：
//! ```text
//! cargo run --manifest-path tools/fuzz/Cargo.toml --release --bin soak -- \
//!     [--iterations N] [--seconds S] [--seed X] [--max-len B] [--quiet]
//! ```
//! 结束时向 stdout 输出一行 JSON 运行统计（供 CI 归档）；任何 panic 以非零码退出。

use std::time::{Duration, Instant};

use ab_fuzz_harness::{fuzz_frame_read, fuzz_rpc_types};

/// 语料种子直接编入二进制（CI 无需路径约定）。
const SEEDS_FRAME: &[&[u8]] = &[
    include_bytes!("../../corpus/frame_read/seed_01_responses.json"),
    include_bytes!("../../corpus/frame_read/seed_02_notifications.json"),
    include_bytes!("../../corpus/frame_read/seed_03_truncated.json"),
    include_bytes!("../../corpus/frame_read/seed_04_binary.bin"),
    include_bytes!("../../corpus/frame_read/seed_05_crlf.json"),
    include_bytes!("../../corpus/frame_read/seed_06_deepnest.json"),
    include_bytes!("../../corpus/frame_read/seed_07_bignumbers.json"),
    include_bytes!("../../corpus/frame_read/seed_08_dupkeys.json"),
    include_bytes!("../../corpus/frame_read/seed_09_typemismatch.json"),
    include_bytes!("../../corpus/frame_read/seed_10_longline.bin"),
];

const SEEDS_TYPES: &[&[u8]] = &[
    include_bytes!("../../corpus/rpc_types/rt_seed_01_initialize.json"),
    include_bytes!("../../corpus/rpc_types/rt_seed_02_canhandle.json"),
    include_bytes!("../../corpus/rpc_types/rt_seed_03_records.json"),
    include_bytes!("../../corpus/rpc_types/rt_seed_04_scalars.json"),
    include_bytes!("../../corpus/rpc_types/rt_seed_05_deepnest.json"),
    include_bytes!("../../corpus/rpc_types/rt_seed_06_bignum.json"),
    include_bytes!("../../corpus/rpc_types/rt_seed_07_dupkeys.json"),
    include_bytes!("../../corpus/rpc_types/rt_seed_08_unicode.json"),
    include_bytes!("../../corpus/rpc_types/rt_seed_09_binary.bin"),
    include_bytes!("../../corpus/rpc_types/rt_seed_10_mixed.json"),
];

// ---------------------------------------------------------------------------
// PRNG（xorshift64*，可复现：--seed 相同 → 语料序列相同）
// ---------------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
}

// ---------------------------------------------------------------------------
// 语料生成：突变 + 拼接 + 原始随机
// ---------------------------------------------------------------------------

/// 经典变异算子：位翻转 / 改字节 / 截断 / 插删 / 块复制 / 随机尾喷。
fn mutate(buf: &mut Vec<u8>, rng: &mut Rng, max_len: usize) {
    let ops = 1 + rng.below(4);
    for _ in 0..ops {
        if buf.is_empty() {
            buf.push(b'A');
        }
        match rng.below(7) {
            0 => {
                let i = rng.below(buf.len());
                buf[i] ^= 1 << rng.below(8);
            }
            1 => {
                let i = rng.below(buf.len());
                buf[i] = rng.next() as u8;
            }
            2 => {
                buf.truncate(rng.below(buf.len() + 1));
            }
            3 => {
                let i = rng.below(buf.len());
                buf.insert(i, rng.next() as u8);
            }
            4 => {
                let i = rng.below(buf.len());
                buf.remove(i);
            }
            5 => {
                // 块复制（放大重复结构：重复键 / 重复批）。
                let a = rng.below(buf.len());
                let b = (a + 1 + rng.below(64)).min(buf.len());
                let chunk: Vec<u8> = buf[a..b].to_vec();
                let at = rng.below(buf.len() + 1);
                buf.splice(at..at, chunk);
            }
            _ => {
                // 随机尾巴：偏向换行 / 引号 / 花括号，提高「完整行」命中率。
                const TAIL: &[u8] = b"\n{}[]\"\\0123456789eE.:-,";
                let n = rng.below(16);
                buf.extend((0..n).map(|_| TAIL[rng.below(TAIL.len())]));
            }
        }
        if buf.len() > max_len {
            buf.truncate(max_len);
        }
    }
}

/// 一次迭代语料：种子突变拼接（帧目标）/ 纯突变（类型目标）/ 原始随机字节。
fn make_input(seeds: &[&[u8]], rng: &mut Rng, max_len: usize) -> Vec<u8> {
    match rng.below(10) {
        0..=2 => {
            // 拼接 1-4 个种子（模拟多帧字节流），逐段突变。
            let parts = 1 + rng.below(4);
            let mut out = Vec::new();
            for _ in 0..parts {
                let mut seg = seeds[rng.below(seeds.len())].to_vec();
                mutate(&mut seg, rng, max_len);
                out.extend_from_slice(&seg);
                if out.len() > max_len {
                    break;
                }
            }
            out.truncate(max_len);
            out
        }
        3..=8 => {
            let mut out = seeds[rng.below(seeds.len())].to_vec();
            mutate(&mut out, rng, max_len);
            out
        }
        _ => (0..rng.below(max_len.min(4096)))
            .map(|_| rng.next() as u8)
            .collect(),
    }
}

// ---------------------------------------------------------------------------
// 定向用例：突变够不着的高成本路径（8MB 上限 / 栈深 / u64 边界）
// ---------------------------------------------------------------------------

fn directed_checks(stats: &mut Stats) {
    // ① 内存有界：9 MB 无换行必须以 LineTooLong 收场（而非 OOM/挂死），
    //    8 MB 内的行必须能走到 JSON 校验（上限不是「静默吞掉」）。
    let cap = 8 * 1024 * 1024;
    let over = vec![b'A'; cap + 1024];
    fuzz_frame_read(&over);
    let ok_line = {
        let mut v = vec![b'1'; cap - 128 * 1024];
        v.push(b'\n');
        v
    };
    fuzz_frame_read(&ok_line); // 大数行 → InvalidJson（不 panic 即不变量）
    stats.directed += 2;

    // ② 栈深：100k 层嵌套必须被 serde_json 递归上限拒绝（harness 内已断言）。
    fuzz_rpc_types(&ab_fuzz_harness::nested_arrays(100_000));
    stats.directed += 1;

    // ③ u64 边界 seq 连发（SeqValidator 内部 last+1 的算术边界）。
    let mut stream = Vec::new();
    for s in [u64::MAX, u64::MAX - 1, 0, u64::MAX] {
        stream.extend_from_slice(
            format!(
                "{{\"jsonrpc\":\"2.0\",\"method\":\"RecordBatch\",\"params\":{{\"file_id\":\"f\",\"seq\":{s},\"records\":[],\"done\":false}}}}\n"
            )
            .as_bytes(),
        );
    }
    fuzz_frame_read(&stream);
    stats.directed += 1;
}

// ---------------------------------------------------------------------------
// 主流程
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Stats {
    iterations: u64,
    frame_inputs: u64,
    type_inputs: u64,
    bytes_fed: u64,
    directed: u64,
    wall: Duration,
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let flag_value = |argv: &[String], i: &mut usize, flag: &str| -> Option<String> {
        if argv[*i] == flag {
            *i += 1;
            argv.get(*i).cloned()
        } else {
            None
        }
    };
    let mut iterations: u64 = 50_000;
    let mut seconds: Option<u64> = None;
    let mut seed: u64 = 0x4142_465A; // "ABFZ"
    let mut max_len: usize = 256 * 1024;
    let mut quiet = false;
    let mut i = 0usize;
    while i < argv.len() {
        let a = argv[i].clone();
        if let Some(v) = flag_value(&argv, &mut i, "--iterations").and_then(|v| v.parse().ok()) {
            iterations = v;
        } else if let Some(v) = flag_value(&argv, &mut i, "--seconds").and_then(|v| v.parse().ok())
        {
            seconds = Some(v);
        } else if let Some(v) = flag_value(&argv, &mut i, "--seed").and_then(|v| v.parse().ok()) {
            seed = v;
        } else if let Some(v) = flag_value(&argv, &mut i, "--max-len").and_then(|v| v.parse().ok())
        {
            max_len = v;
        } else if a == "--quiet" {
            quiet = true;
        } else {
            eprintln!("unknown arg {a}");
            std::process::exit(2);
        }
        i += 1;
    }

    let t0 = Instant::now();
    let mut stats = Stats::default();
    let mut rng = Rng::new(seed);
    let mut next_report = 1u64;

    for i in 0..iterations {
        let input = make_input(SEEDS_FRAME, &mut rng, max_len);
        stats.bytes_fed += input.len() as u64;
        fuzz_frame_read(&input);
        stats.frame_inputs += 1;

        let input = make_input(SEEDS_TYPES, &mut rng, max_len);
        stats.bytes_fed += input.len() as u64;
        fuzz_rpc_types(&input);
        stats.type_inputs += 1;

        stats.iterations = i + 1;
        if let Some(s) = seconds {
            if t0.elapsed().as_secs() >= s {
                break;
            }
        }
        if !quiet && t0.elapsed().as_secs() >= next_report {
            next_report = t0.elapsed().as_secs() + 5;
            eprintln!(
                "[soak] iter {} frame+type, {} bytes fed, {:?}",
                stats.iterations,
                stats.bytes_fed,
                t0.elapsed()
            );
        }
    }

    directed_checks(&mut stats);
    stats.wall = t0.elapsed();

    println!(
        "{{\"tool\":\"ab-fuzz-soak\",\"mode\":\"stable-loop\",\"iterations\":{},\"frame_inputs\":{},\"type_inputs\":{},\"directed_cases\":{},\"bytes_fed\":{},\"seed\":{},\"wall_sec\":{:.1},\"status\":\"survived\"}}",
        stats.iterations,
        stats.frame_inputs,
        stats.type_inputs,
        stats.directed,
        stats.bytes_fed,
        seed,
        stats.wall.as_secs_f32()
    );
}
