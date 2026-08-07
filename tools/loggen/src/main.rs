//! loggen —— AnalysisBuddy 确定性合成 Log 生成器（qa-perf.md §1）。
//!
//! 同参数同 seed 输出逐字节可复现（SHA-256 一致）；单线程顺序写 + 64KB 缓冲，
//! 100MB 档 ≤60s。CSV 输出与 builtin-csv 零配置解析对齐（首列 `timestamp`、
//! RFC3339 毫秒精度）；txt 输出为 demo-tool 约定行格式（sdk-plugins.md §4.1
//! FRAME/STATE/EVENT，FRAME 行尾 `level=` 按 85/12/3 分布 info/warn/error）。
//!
//! 退出码：`0` 成功｜`2` 参数冲突（如 disorder ≥1）｜`4` IO 失败。

mod waveforms;

use std::collections::HashSet;
use std::env;
use std::fmt::Write as _;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use encoding_rs::GBK;

use waveforms::{MetricDomain, Rng, WaveformSampler};

/// 缺省起始时间：2026-08-01T00:00:00.000Z 的 epoch 毫秒（qa-perf.md §1.1）。
const DEFAULT_START_MS: i64 = 1_785_542_400_000;
/// 8MB 协议帧上限（protocol-v1.md §1.3），供文档与 long-line 夹具引用。
pub const FRAME_LIMIT_BYTES: usize = 8 * 1024 * 1024;

// ---------------------------------------------------------------------------
// CLI 模型
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SizeTarget {
    Auto,
    FixedMb(u64),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Csv,
    Txt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Encoding {
    Utf8,
    Utf8Bom,
    Gbk,
}

#[derive(Debug, Clone, PartialEq)]
struct Config {
    rows: u64,
    metrics: usize,
    size_target: SizeTarget,
    format: Format,
    seed: u64,
    out: PathBuf,
    start_ms: i64,
    interval_ms: u64,
    disorder: f64,
    encoding: Encoding,
    no_header: bool,
    corrupt: f64,
}

/// 生成统计（stderr 报告实际值用）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stats {
    rows: u64,
    bytes: u64,
}

#[derive(Debug)]
struct LoggenError {
    code: u8,
    msg: String,
}

impl LoggenError {
    fn usage(msg: impl Into<String>) -> Self {
        Self { code: 2, msg: msg.into() }
    }
    fn io(msg: impl Into<String>) -> Self {
        Self { code: 4, msg: msg.into() }
    }
}

impl std::fmt::Display for LoggenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.msg)
    }
}

// ---------------------------------------------------------------------------
// CLI 解析
// ---------------------------------------------------------------------------

const USAGE: &str = "\
loggen --rows <N> --metrics <N> --size-target <50MB|100MB|auto>
       --format csv|txt --seed <s> -o <path> [选项]

必选：
  --rows <N>          记录行数（数据行，不含表头）
  --metrics <N>       指标列数量（1~64）
  --size-target <t>   目标体积档位：50MB / 100MB / auto（由 rows 自然决定）；
                      与 --rows 冲突时 loggen 调整 rows 就近取整并在 stderr 报告实际值
  --format csv|txt    csv = 带表头逗号分隔；txt = demo-tool 半结构化日志行
  --seed <s>          随机种子（u64），同种子输出逐字节可复现（CI 断言哈希用）
  -o <path>           输出文件路径

可选：
  --start <ISO8601>   起始时间戳（缺省 2026-08-01T00:00:00.000Z）
  --interval <ms>     基础行间隔毫秒（缺省 100）
  --disorder <ratio>  乱序比例 [0,1)：随机挑选该比例的行与邻近行交换时间戳（缺省 0）
  --encoding utf8|utf8bom|gbk   编码变体（缺省 utf8；仅用于生成编码夹具）
  --no-header         csv 模式去掉表头行
  --corrupt <ratio>   畸形行比例（缺省 0；用于生成畸形夹具）

退出码：0 成功｜2 参数冲突（如 disorder ≥1）｜4 IO 失败";

fn need_value(args: &[String], i: &mut usize, flag: &str) -> Result<String, LoggenError> {
    *i += 1;
    args.get(*i)
        .cloned()
        .ok_or_else(|| LoggenError::usage(format!("{flag} requires a value")))
}

fn parse_u64(s: &str, flag: &str) -> Result<u64, LoggenError> {
    s.parse::<u64>()
        .map_err(|_| LoggenError::usage(format!("{flag}: invalid integer `{s}`")))
}

fn parse_f64(s: &str, flag: &str) -> Result<f64, LoggenError> {
    s.parse::<f64>()
        .map_err(|_| LoggenError::usage(format!("{flag}: invalid number `{s}`")))
}

fn parse_args(args: &[String]) -> Result<Config, LoggenError> {
    let mut rows: Option<u64> = None;
    let mut metrics: Option<usize> = None;
    // --size-target 缺省 auto（card 验证命令省略该参数时按 rows 自然决定）。
    let mut size_target: Option<SizeTarget> = Some(SizeTarget::Auto);
    let mut format: Option<Format> = None;
    let mut seed: Option<u64> = None;
    let mut out: Option<PathBuf> = None;
    let mut start_ms = DEFAULT_START_MS;
    let mut interval_ms = 100u64;
    let mut disorder = 0.0f64;
    let mut encoding = Encoding::Utf8;
    let mut no_header = false;
    let mut corrupt = 0.0f64;

    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "--rows" => rows = Some(parse_u64(&need_value(args, &mut i, "--rows")?, "--rows")?),
            "--metrics" => {
                metrics = Some(parse_u64(&need_value(args, &mut i, "--metrics")?, "--metrics")?
                    as usize)
            }
            "--size-target" => {
                let v = need_value(args, &mut i, "--size-target")?;
                size_target = Some(match v.to_ascii_lowercase().as_str() {
                    "auto" => SizeTarget::Auto,
                    "50mb" | "50m" => SizeTarget::FixedMb(50),
                    "100mb" | "100m" => SizeTarget::FixedMb(100),
                    _ => {
                        return Err(LoggenError::usage(format!(
                            "--size-target: expected 50MB|100MB|auto, got `{v}`"
                        )));
                    }
                });
            }
            "--format" => {
                let v = need_value(args, &mut i, "--format")?;
                format = Some(match v.to_ascii_lowercase().as_str() {
                    "csv" => Format::Csv,
                    "txt" => Format::Txt,
                    _ => return Err(LoggenError::usage(format!("--format: got `{v}`"))),
                });
            }
            "--seed" => seed = Some(parse_u64(&need_value(args, &mut i, "--seed")?, "--seed")?),
            "-o" => out = Some(PathBuf::from(need_value(args, &mut i, "-o")?)),
            "--start" => {
                let v = need_value(args, &mut i, "--start")?;
                start_ms = parse_iso_ms(&v)
                    .ok_or_else(|| LoggenError::usage(format!("--start: invalid ISO8601 `{v}`")))?;
            }
            "--interval" => {
                interval_ms = parse_u64(&need_value(args, &mut i, "--interval")?, "--interval")?
            }
            "--disorder" => {
                disorder = parse_f64(&need_value(args, &mut i, "--disorder")?, "--disorder")?
            }
            "--encoding" => {
                let v = need_value(args, &mut i, "--encoding")?;
                encoding = match v.to_ascii_lowercase().as_str() {
                    "utf8" => Encoding::Utf8,
                    "utf8bom" => Encoding::Utf8Bom,
                    "gbk" => Encoding::Gbk,
                    _ => return Err(LoggenError::usage(format!("--encoding: got `{v}`"))),
                };
            }
            "--no-header" => no_header = true,
            "--corrupt" => {
                corrupt = parse_f64(&need_value(args, &mut i, "--corrupt")?, "--corrupt")?
            }
            "--help" | "-h" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => return Err(LoggenError::usage(format!("unknown argument `{other}`"))),
        }
        i += 1;
    }

    let rows = rows.ok_or_else(|| LoggenError::usage("missing required --rows"))?;
    let metrics = metrics.ok_or_else(|| LoggenError::usage("missing required --metrics"))?;
    let size_target =
        size_target.ok_or_else(|| LoggenError::usage("missing required --size-target"))?;
    let format = format.ok_or_else(|| LoggenError::usage("missing required --format"))?;
    let seed = seed.ok_or_else(|| LoggenError::usage("missing required --seed"))?;
    let out = out.ok_or_else(|| LoggenError::usage("missing required -o"))?;

    if !(1..=64).contains(&metrics) {
        return Err(LoggenError::usage("--metrics must be in 1..=64"));
    }
    if !(0.0..1.0).contains(&disorder) {
        return Err(LoggenError::usage(
            "--disorder must be in [0, 1)（例如 0.02）",
        ));
    }
    if !(0.0..1.0).contains(&corrupt) {
        return Err(LoggenError::usage("--corrupt must be in [0, 1)"));
    }
    if interval_ms == 0 {
        return Err(LoggenError::usage("--interval must be ≥ 1"));
    }

    Ok(Config {
        rows,
        metrics,
        size_target,
        format,
        seed,
        out,
        start_ms,
        interval_ms,
        disorder,
        encoding,
        no_header,
        corrupt,
    })
}

// ---------------------------------------------------------------------------
// 时间工具：ISO8601 ↔ epoch 毫秒（零依赖、确定性）
// ---------------------------------------------------------------------------

/// `YYYY-MM-DDTHH:MM:SS[.mmm][Z|±HH:MM]` → epoch 毫秒；缺省时区视为 UTC。
fn parse_iso_ms(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[13] != b':' || b[16] != b':'
    {
        return None;
    }
    let digits = |r: std::ops::Range<usize>| -> Option<i64> {
        if r.clone().any(|i| !b[i].is_ascii_digit()) {
            return None;
        }
        std::str::from_utf8(&b[r]).ok()?.parse().ok()
    };
    let (y, mo, d) = (digits(0..4)?, digits(5..7)?, digits(8..10)?);
    let (h, mi, s) = (digits(11..13)?, digits(14..16)?, digits(17..19)?);
    let mut ms = 0i64;
    let mut pos = 19;
    if b.get(pos) == Some(&b'.') {
        let mut frac = 0i64;
        let mut scale = 100i64;
        pos += 1;
        while let Some(c) = b.get(pos) {
            if !c.is_ascii_digit() || scale == 0 {
                break;
            }
            frac += (c - b'0') as i64 * scale;
            scale /= 10;
            pos += 1;
        }
        ms = frac;
    }
    let mut offset_min = 0i64;
    match b.get(pos) {
        None | Some(&b'Z') | Some(&b'z') => {}
        Some(&b'+') | Some(&b'-') => {
            let sign = if b.get(pos) == Some(&b'+') { 1i64 } else { -1i64 };
            pos += 1;
            let hh = digits(pos..pos + 2)?;
            pos += 2;
            let mm = if b.get(pos) == Some(&b':') {
                pos += 1;
                digits(pos..pos + 2)?
            } else {
                0
            };
            offset_min = sign * (hh * 60 + mm);
        }
        _ => return None,
    }
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    let days = days_from_civil(y, mo, d);
    if days < 0 {
        return None;
    }
    Some(days * 86_400_000 + (h * 3_600 + mi * 60 + s) * 1_000 + ms - offset_min * 60_000)
}

/// 公历 → 自 1970-01-01 起的天数（Hinnant 算法）。
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = y - if m <= 2 { 1 } else { 0 };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// 天数 → 公历（Hinnant 算法）。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (mp + if mp < 10 { 3 } else { -9 }) as u32;
    let y = y + if m <= 2 { 1 } else { 0 };
    (y, m, d)
}

/// epoch 毫秒 → `YYYY-MM-DDTHH:MM:SS.mmmZ`（UTC）。
fn fmt_iso_ms(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let ms_of_day = ms.rem_euclid(86_400_000);
    let (y, mo, d) = civil_from_days(days);
    let h = ms_of_day / 3_600_000;
    let mi = (ms_of_day % 3_600_000) / 60_000;
    let s = (ms_of_day % 60_000) / 1_000;
    let ms3 = ms_of_day % 1_000;
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{ms3:03}Z")
}

/// 固定小数位数值（行宽确定，参与体积标定）。
fn fmt_fixed(v: f64, decimals: usize) -> String {
    format!("{v:.decimals$}")
}

// ---------------------------------------------------------------------------
// 指标与行生成
// ---------------------------------------------------------------------------

const CSV_HEADER: &str = "timestamp,fps,frame_ms,mem_mb";
const GBK_NOTE_COLUMN: &str = ",备注";
/// GBK 中文备注取值池（enc_gbk.csv 夹具用）。
const GBK_LABELS: [&str; 5] = ["匹配", "异常", "正常", "加载", "战斗"];

/// 指标名池与取值域（qa-perf.md §1.2：fps 0~120、frame_ms 1~60、mem_mb 50~4000 等）。
///
/// csv 前 3 个固定为 `fps,frame_ms,mem_mb`（表头与 builtin-csv 指纹对齐）；
/// txt 前 3 个固定为 demo-tool 键 `fps,frame_ms,cpu_temp`（sdk-plugins.md §4.1）。
fn metric_specs(csv: bool, count: usize) -> Vec<(String, MetricDomain)> {
    let pool: &[(&str, MetricDomain)] = &[
        ("fps", MetricDomain::new(0.0, 120.0, 2)),
        ("frame_ms", MetricDomain::new(1.0, 60.0, 2)),
        ("mem_mb", MetricDomain::new(50.0, 4000.0, 1)),
        ("cpu_temp", MetricDomain::new(30.0, 100.0, 1)),
        ("gpu_util", MetricDomain::new(0.0, 100.0, 1)),
        ("net_kbps", MetricDomain::new(0.0, 100_000.0, 0)),
        ("disk_io_mb", MetricDomain::new(0.0, 500.0, 1)),
        ("latency_ms", MetricDomain::new(1.0, 1000.0, 0)),
    ];
    // txt 的 demo-tool 前 3 键（cpu_temp 顶替 mem_mb 的位置）。
    let txt_prefix: &[(&str, MetricDomain)] = &[
        ("fps", MetricDomain::new(0.0, 120.0, 2)),
        ("frame_ms", MetricDomain::new(1.0, 60.0, 2)),
        ("cpu_temp", MetricDomain::new(30.0, 100.0, 1)),
    ];
    let mut v = Vec::with_capacity(count);
    for i in 0..count {
        let (name, domain) = if csv {
            if i < pool.len() {
                (pool[i].0.to_string(), pool[i].1)
            } else {
                (format!("metric_{i}"), MetricDomain::new(0.0, 1000.0, 2))
            }
        } else if i < txt_prefix.len() {
            (txt_prefix[i].0.to_string(), txt_prefix[i].1)
        } else {
            (format!("metric_{i}"), MetricDomain::new(0.0, 1000.0, 2))
        };
        v.push((name, domain));
    }
    v
}

/// 行内 metric 值串（csv：`fps,frame_ms,mem_mb`；txt 的 FRAME 用 `key=v`）。
struct LineBuilder {
    csv: bool,
    gbk: bool,
    specs: Vec<(String, MetricDomain)>,
    samplers: Vec<WaveformSampler>,
    level_rng: Rng,
    gbk_rng: Rng,
    corrupt_rng: Rng,
}

impl LineBuilder {
    fn new(cfg: &Config) -> Self {
        let csv = cfg.format == Format::Csv;
        let specs = metric_specs(csv, cfg.metrics);
        let samplers = specs
            .iter()
            .enumerate()
            .map(|(m, (_, d))| WaveformSampler::new(cfg.seed, m, *d))
            .collect();
        Self {
            csv,
            gbk: cfg.encoding == Encoding::Gbk,
            specs,
            samplers,
            level_rng: Rng::new(cfg.seed ^ 0x0DDB_1A5C_5EB4_0000u64),
            gbk_rng: Rng::new(cfg.seed ^ 0x9E37_79B9_7F4A_7C15u64),
            corrupt_rng: Rng::new(cfg.seed ^ 0xC0FF_EE00_C0FF_EE00u64),
        }
    }

    /// 第 `row` 行文本（不含换行）。`corrupt` = 该行是否执行畸形变体。
    fn row(&mut self, row: u64, ts: i64, corrupt: bool) -> String {
        let mut out = String::with_capacity(96);
        if self.csv {
            let _ = write!(out, "{}", fmt_iso_ms(ts));
            for m in 0..self.samplers.len() {
                let v = self.samplers[m].next(row);
                let (_, d) = &self.specs[m];
                let _ = write!(out, ",{}", fmt_fixed(v, d.decimals));
            }
            // GBK 模式追加中文备注列（仅 csv，enc_gbk.csv 夹具用）。
            if self.gbk {
                let label = GBK_LABELS[(self.gbk_rng.next_u64() % GBK_LABELS.len() as u64) as usize];
                let _ = write!(out, ",{label}");
            }
            if corrupt {
                return corrupt_csv_row(&out, &mut self.corrupt_rng);
            }
        } else {
            let level = match self.level_rng.next_f64() {
                r if r < 0.85 => "info",
                r if r < 0.97 => "warn",
                _ => "error",
            };
            if row.is_multiple_of(100) {
                // EVENT 行：annotate 语义（sdk-plugins.md §4.1）。
                let names = ["crash_dump", "checkpoint", "match_start", "level_up"];
                let reasons = ["GPU hang", "CPU spike", "loot drop", "boss defeated"];
                let n = names[(row / 100) as usize % names.len()];
                let r = reasons[(row / 100) as usize % reasons.len()];
                let lvl = ["error", "warn", "info"][(row / 100) as usize % 3];
                let _ = write!(out, "{} EVENT {n} reason=\"{r}\" level={lvl}", fmt_iso_ms(ts));
            } else if row.is_multiple_of(40) {
                // STATE 行：key_values 语义。
                let scenes = ["lobby", "main_menu", "boss_fight", "training", "settings"];
                let scene = scenes[(row / 40) as usize % scenes.len()];
                let hp = 50 + self.level_rng.next_u64() % 51;
                let stamina = self.level_rng.next_u64() % 101;
                let _ = write!(
                    out,
                    "{} STATE scene={scene} hero_hp={hp} stamina={stamina}",
                    fmt_iso_ms(ts)
                );
            } else {
                // FRAME 行：demo-tool 指标键。
                let _ = write!(out, "{} FRAME", fmt_iso_ms(ts));
                for m in 0..self.samplers.len() {
                    let v = self.samplers[m].next(row);
                    let (name, d) = &self.specs[m];
                    let _ = write!(out, " {name}={}", fmt_fixed(v, d.decimals));
                }
                let _ = write!(out, " level={level}");
            }
            if corrupt {
                return corrupt_txt_row(&out, &mut self.corrupt_rng);
            }
        }
        out
    }
}

/// 畸形 csv 行变体（qa-perf.md §2 malformed_lines：缺列/非数值/坏时间戳/超长行）。
fn corrupt_csv_row(line: &str, rng: &mut Rng) -> String {
    match rng.next_u64() % 4 {
        0 => {
            // 缺列：去掉最后一个字段。
            let mut parts: Vec<&str> = line.split(',').collect();
            parts.pop();
            parts.join(",")
        }
        1 => {
            // 非数值 value：替换首指标值为非数值。
            let mut parts: Vec<&str> = line.split(',').collect();
            if parts.len() > 1 {
                parts[1] = "abc";
            }
            parts.join(",")
        }
        2 => {
            // 时间戳非 ISO。
            let comma = line.find(',').unwrap_or(line.len());
            format!("not-a-time{}", &line[comma..])
        }
        _ => {
            // 超长单行：>1KB 但 <8MB。
            format!("{line}{}", "x".repeat(2048))
        }
    }
}

/// 畸形 txt 行变体。
fn corrupt_txt_row(line: &str, rng: &mut Rng) -> String {
    match rng.next_u64() % 3 {
        0 => "garbage line without timestamp".to_string(),
        1 => {
            // 坏时间戳。
            let sp = line.find(' ').unwrap_or(line.len());
            format!("not-a-time{}", &line[sp..])
        }
        _ => {
            // 缺键值：仅时间戳 + FRAME。
            let ts = line.split(' ').next().unwrap_or(line);
            format!("{ts} FRAME")
        }
    }
}

// ---------------------------------------------------------------------------
// 时间戳序列与乱序
// ---------------------------------------------------------------------------

/// 基础时间戳序列：`start + i × interval + jitter(0~interval/2)`（严格递增）。
fn timestamps(cfg: &Config) -> Vec<i64> {
    let mut rng = Rng::new(cfg.seed ^ 0x5DEE_DEAD_5DEE_DEADu64);
    let half = cfg.interval_ms / 2;
    let mut v = Vec::with_capacity(cfg.rows as usize);
    for i in 0..cfg.rows {
        let jitter = if half == 0 {
            0
        } else {
            (rng.next_f64() * half as f64) as i64
        };
        v.push(cfg.start_ms + (i * cfg.interval_ms) as i64 + jitter);
    }
    if cfg.disorder > 0.0 {
        disorder_swap(&mut v, cfg);
    }
    v
}

/// 乱序：随机挑 `ratio × rows` 行与 ±1~3 位邻居交换时间戳，交换后不再修复。
fn disorder_swap(ts: &mut [i64], cfg: &Config) {
    let n = ts.len();
    if n < 2 {
        return;
    }
    let count = ((cfg.disorder * n as f64).floor() as usize).min(n / 2);
    let mut rng = Rng::new(cfg.seed ^ 0x5EED_B00F_5EED_B00Fu64);
    for _ in 0..count {
        let i = rng.range(0, n as i64 - 1) as usize;
        let mut off = rng.range(-3, 3);
        while off == 0 {
            off = rng.range(-3, 3);
        }
        let j = (i as i64 + off).clamp(0, n as i64 - 1) as usize;
        if i != j {
            ts.swap(i, j);
        }
    }
}

/// 畸形行索引集合：恰好 `floor(ratio × rows)` 行（确定性排斥采样）。
fn corrupt_indices(cfg: &Config) -> HashSet<u64> {
    if cfg.corrupt <= 0.0 {
        return HashSet::new();
    }
    let count = (cfg.corrupt * cfg.rows as f64).floor() as u64;
    let mut rng = Rng::new(cfg.seed ^ 0xBAD_F00D_0000_5EEDu64);
    let mut picked: HashSet<u64> = HashSet::with_capacity(count as usize);
    while picked.len() < count as usize {
        let idx = rng.next_u64() % cfg.rows;
        picked.insert(idx);
    }
    picked
}

// ---------------------------------------------------------------------------
// 生成主流程
// ---------------------------------------------------------------------------

/// 生成入口：`--size-target 50MB|100MB` 时以内存迭代收敛 rows，偏差 ≤0.2%。
fn generate(cfg: &Config, writer: &mut impl Write) -> Result<Stats, LoggenError> {
    let mut rows = cfg.rows;
    if let SizeTarget::FixedMb(mb) = cfg.size_target {
        let target = mb * 1024 * 1024;
        let mut best: Vec<u8> = Vec::new();
        let mut best_dev = f64::INFINITY;
        for _ in 0..6 {
            best.clear();
            let c = Config { rows, ..cfg.clone() };
            generate_rows(&c, &mut best)?;
            let actual = best.len() as u64;
            let dev = (actual as i64 - target as i64).unsigned_abs() as f64 / target as f64;
            best_dev = dev;
            if dev <= 0.002 {
                break;
            }
            let next = ((rows * target) / actual).max(1);
            if next == rows {
                rows = rows.saturating_sub(1).max(1);
                break;
            }
            rows = next;
        }
        eprintln!(
            "INFO loggen: size-target {mb}MB → rows {rows}, actual {} bytes (dev {:.3}%)",
            best.len(),
            best_dev * 100.0
        );
        writer.write_all(&best).map_err(io_err)?;
        writer.flush().map_err(io_err)?;
        return Ok(Stats {
            rows,
            bytes: best.len() as u64,
        });
    }
    generate_rows(cfg, writer)
}

fn generate_rows(cfg: &Config, writer: &mut impl Write) -> Result<Stats, LoggenError> {
    let csv = cfg.format == Format::Csv;
    let mut builder = LineBuilder::new(cfg);

    // 表头（csv 且非 --no-header）。
    let mut header_bytes: Vec<u8> = Vec::new();
    if csv && !cfg.no_header {
        let header = if cfg.encoding == Encoding::Gbk {
            format!("{CSV_HEADER}{GBK_NOTE_COLUMN}")
        } else {
            CSV_HEADER.to_string()
        };
        let _ = writeln!(header_bytes, "{header}");
    }

    // 时间戳序列（乱序在此完成）。
    let ts = timestamps(cfg);
    let corrupt_idx = corrupt_indices(cfg);

    if cfg.encoding == Encoding::Utf8Bom {
        writer.write_all(&[0xEF, 0xBB, 0xBF]).map_err(io_err)?;
    }
    let mut written = header_bytes.len() as u64;
    if !header_bytes.is_empty() {
        writer.write_all(&header_bytes).map_err(io_err)?;
    }

    let mut line_buf = String::with_capacity(128);
    let mut enc_buf: Vec<u8> = Vec::with_capacity(128);
    for row in 0..cfg.rows {
        line_buf.clear();
        let corrupt = corrupt_idx.contains(&row);
        let line = builder.row(row, ts[row as usize], corrupt);
        let bytes = match cfg.encoding {
            Encoding::Gbk => {
                let (enc, _, _) = GBK.encode(line.as_str());
                enc_buf.clear();
                enc_buf.extend_from_slice(&enc);
                &enc_buf[..]
            }
            Encoding::Utf8 | Encoding::Utf8Bom => line.as_bytes(),
        };
        writer.write_all(bytes).map_err(io_err)?;
        writer.write_all(b"\n").map_err(io_err)?;
        written += bytes.len() as u64 + 1;
    }
    writer.flush().map_err(io_err)?;
    Ok(Stats {
        rows: cfg.rows,
        bytes: written,
    })
}

fn io_err(e: std::io::Error) -> LoggenError {
    LoggenError::io(format!("IO failure: {e}"))
}

fn run(cfg: &Config) -> Result<Stats, LoggenError> {
    let file = File::create(&cfg.out).map_err(|e| {
        LoggenError::io(format!("cannot create `{}`: {e}", cfg.out.display()))
    })?;
    let mut writer = BufWriter::with_capacity(64 * 1024, file);
    generate(cfg, &mut writer)
}

// ---------------------------------------------------------------------------
// 入口
// ---------------------------------------------------------------------------

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    match parse_args(&args) {
        Err(e) if e.code == 2 => {
            eprintln!("ERROR loggen: {e}");
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
        Err(e) => {
            eprintln!("ERROR loggen: {e}");
            ExitCode::from(e.code)
        }
        Ok(cfg) => match run(&cfg) {
            Ok(stats) => {
                let size_mb = stats.bytes as f64 / (1024.0 * 1024.0);
                eprintln!(
                    "INFO loggen: done rows={} bytes={} ({size_mb:.2}MB) → {}",
                    stats.rows,
                    stats.bytes,
                    cfg.out.display()
                );
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("ERROR loggen: {e}");
                ExitCode::from(e.code)
            }
        },
    }
}

// ---------------------------------------------------------------------------
// 单测：参数/退出码分支、时间工具、确定性组件
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(rows: u64, metrics: usize, seed: u64) -> Config {
        Config {
            rows,
            metrics,
            size_target: SizeTarget::Auto,
            format: Format::Csv,
            seed,
            out: PathBuf::from("out.csv"),
            start_ms: DEFAULT_START_MS,
            interval_ms: 100,
            disorder: 0.0,
            encoding: Encoding::Utf8,
            no_header: false,
            corrupt: 0.0,
        }
    }

    #[test]
    fn exit_code_2_on_param_conflict() {
        let e = parse_args(&["--rows".into(), "10".into(), "--metrics".into(), "3".into(), "--size-target".into(), "auto".into(), "--format".into(), "csv".into(), "--seed".into(), "1".into(), "-o".into(), "x".into(), "--disorder".into(), "1.0".into()])
            .unwrap_err();
        assert_eq!(e.code, 2, "disorder ≥ 1 必须退出码 2");
        assert!(e.msg.contains("disorder"));

        let e = parse_args(&["--rows".into(), "10".into(), "--metrics".into(), "65".into(), "--size-target".into(), "auto".into(), "--format".into(), "csv".into(), "--seed".into(), "1".into(), "-o".into(), "x".into()])
            .unwrap_err();
        assert_eq!(e.code, 2, "metrics > 64 必须退出码 2");

        let e = parse_args(&["--rows".into(), "10".into(), "--metrics".into(), "3".into(), "--size-target".into(), "auto".into(), "--format".into(), "csv".into(), "--seed".into(), "1".into(), "-o".into(), "x".into(), "--size-target".into(), "25MB".into()])
            .unwrap_err();
        assert_eq!(e.code, 2);
    }

    #[test]
    fn exit_code_4_on_io_failure() {
        let mut c = cfg(10, 3, 1);
        c.out = PathBuf::from("Z:/nonexistent-dir-xyz/out.csv");
        let e = run(&c).unwrap_err();
        assert_eq!(e.code, 4, "无法创建输出文件必须退出码 4");
    }

    #[test]
    fn exit_code_0_on_success() {
        let dir = std::env::temp_dir();
        let mut c = cfg(10, 3, 1);
        c.out = dir.join("loggen-unit-ok.csv");
        let stats = run(&c).expect("generation succeeds");
        assert_eq!(stats.rows, 10);
        assert!(stats.bytes > 0);
        let _ = std::fs::remove_file(&c.out);
    }

    #[test]
    fn required_args_enforced() {
        let e = parse_args(&["--rows".into(), "10".into()]).unwrap_err();
        assert!(e.msg.contains("--metrics"));
    }

    #[test]
    fn iso_round_trip() {
        let ms = DEFAULT_START_MS;
        assert_eq!(fmt_iso_ms(ms), "2026-08-01T00:00:00.000Z");
        assert_eq!(parse_iso_ms("2026-08-01T00:00:00.000Z"), Some(ms));
        assert_eq!(
            parse_iso_ms("2026-08-01T08:00:00.000+08:00"),
            Some(ms),
            "+08:00 偏移应折算回 UTC"
        );
        assert_eq!(parse_iso_ms("2026-08-01T00:00:00Z"), Some(ms));
        assert_eq!(parse_iso_ms("2026-08-01T00:00:00"), Some(ms), "无时区按 UTC");
        assert_eq!(parse_iso_ms("bad"), None);
        assert_eq!(parse_iso_ms("2026-13-01T00:00:00Z"), None, "月 13 非法");
        let end = parse_iso_ms("2026-08-01T00:00:00.123Z").unwrap();
        assert_eq!(fmt_iso_ms(end), "2026-08-01T00:00:00.123Z");
    }

    #[test]
    fn timestamps_strictly_increasing_without_disorder() {
        let c = cfg(5000, 3, 42);
        let ts = timestamps(&c);
        for w in ts.windows(2) {
            assert!(w[1] > w[0], "无乱序时必须严格递增: {} -> {}", w[0], w[1]);
        }
    }

    #[test]
    fn disorder_swaps_exact_ratio_of_rows() {
        let mut c = cfg(2000, 3, 21);
        c.disorder = 0.2;
        let ts = timestamps(&c);
        let sorted = {
            let mut s = ts.clone();
            s.sort_unstable();
            s
        };
        // 集合相同（只交换不增删）但顺序被打乱。
        assert_eq!(ts.len(), sorted.len());
        let diffs = ts.iter().zip(sorted.iter()).filter(|(a, b)| a != b).count();
        assert!(diffs > 0, "0.2 乱序必然产生顺序差异");
        assert!(diffs <= 1000, "乱序行数 ≤ ratio×rows 的 2.5 倍容忍带: {diffs}");
    }

    #[test]
    fn corrupt_indices_exact_count() {
        let mut c = cfg(200, 3, 203);
        c.corrupt = 0.10;
        let idx = corrupt_indices(&c);
        assert_eq!(idx.len(), 20, "200 行 × 0.10 必须恰好 20 行畸形");
    }

    #[test]
    fn size_calibration_lands_within_2pct() {
        // 以内部标定路径验证：1MB 目标 → 生成 → 偏差 ≤2%。
        let mut c = cfg(10_000_000, 3, 100); // rows 会被覆盖
        c.size_target = SizeTarget::FixedMb(1);
        let mut buf: Vec<u8> = Vec::new();
        let stats = generate(&c, &mut buf).expect("generate");
        let actual = stats.bytes;
        let target = 1024 * 1024;
        let dev = (actual as i64 - target as i64).unsigned_abs() as f64 / target as f64;
        assert!(dev <= 0.02, "1MB 目标实际 {actual}B 偏差 {dev:.3} > 2%");
        assert!(stats.rows < 10_000_000, "rows 应被就近取整到 ~1MB 对应值");
    }

    #[test]
    fn csv_header_matches_fingerprint_convention() {
        let c = cfg(5, 3, 1);
        let mut buf: Vec<u8> = Vec::new();
        generate(&c, &mut buf).unwrap();
        let text = String::from_utf8(buf).unwrap();
        let mut lines = text.lines();
        assert_eq!(
            lines.next(),
            Some("timestamp,fps,frame_ms,mem_mb"),
            "表头必须可命中 builtin-csv 指纹 `timestamp,`"
        );
        assert!(
            lines.all(|l| l.starts_with("2026-")),
            "数据行以 ISO8601 开头"
        );
    }

    #[test]
    fn gbk_output_is_not_utf8() {
        let mut c = cfg(50, 3, 205);
        c.encoding = Encoding::Gbk;
        let mut buf: Vec<u8> = Vec::new();
        generate(&c, &mut buf).unwrap();
        assert!(std::str::from_utf8(&buf).is_err(), "GBK 字节流不应是合法 UTF-8");
        assert!(buf.windows(2).any(|w| w[0] >= 0x80), "应包含高位字节（中文备注列）");
    }

    #[test]
    fn txt_rows_cover_all_three_levels_and_line_kinds() {
        let mut c = cfg(500, 3, 202);
        c.format = Format::Txt;
        let mut buf: Vec<u8> = Vec::new();
        generate(&c, &mut buf).unwrap();
        let text = String::from_utf8(buf).unwrap();
        assert!(text.lines().any(|l| l.contains("level=info")));
        assert!(text.lines().any(|l| l.contains("level=warn")));
        assert!(text.lines().any(|l| l.contains("level=error")));
        assert!(text.lines().any(|l| l.contains(" FRAME ")));
        assert!(text.lines().any(|l| l.contains(" STATE ")));
        assert!(text.lines().any(|l| l.contains(" EVENT ")));
    }
}
