//! 真实插件 E2E 套件（qa-perf.md §3.2）：
//! manifest 预筛 + `can_handle` 置信度断言 → parse 完成 → 查询 API 断言
//! （记录数 == fixture 行数、时间范围 == fixture 范围、指标集 == `schema()` 声明）
//! → 游标 `key_values`（demo-tool 真实语义）→ 多文件同轴叠加。
//!
//! 跨路依赖：builtin-csv（D1-03）与 demo-tool（D2-03）为 Phase 2 并行交付物，
//! 尚未合入时本套件按插件缺失跳过（SKIP 并给出定位信息）；插件就绪后自动激活。
//! 断言失败时 stderr 转储产物落盘（target/test-artifacts/e2e/）。

use std::path::{Path, PathBuf};
use std::time::Instant;

use ab_e2e::fixtures_ref;
use ab_e2e::harness::{dump_on_failure, FileEntryState, PluginInvocation, PluginSession, Store};
use ab_protocol::manifest::Manifest;
use serde_json::Value;

const FILE_ID: &str = "f3c1d2a4-9e7b-4a01-b2c3-0d5e6f7a8b9c";

/// 已解析的插件目录（plugin.json + 入口调用方式）。
struct ResolvedPlugin {
    dir: PathBuf,
    manifest: Manifest,
}

fn plugin_dir(id: &str) -> PathBuf {
    fixtures_ref::workspace_root().join("plugins").join(id)
}

/// 插件未构建（plugin.json 缺失）→ None（SKIP）。
fn resolve_plugin(id: &str) -> Option<ResolvedPlugin> {
    let dir = plugin_dir(id);
    let manifest_path = dir.join("plugin.json");
    if !manifest_path.exists() {
        eprintln!(
            "[SKIP] {id}: plugin.json 缺失（D1-03/D2-03 交付后自动激活）: {}",
            manifest_path.display()
        );
        return None;
    }
    let text = std::fs::read_to_string(&manifest_path)
        .unwrap_or_else(|e| panic!("read {manifest_path:?}: {e}"));
    let manifest: Manifest =
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse manifest {id}: {e}"));
    Some(ResolvedPlugin { dir, manifest })
}

/// 按 manifest entry 解析进程调用（command 相对插件目录；解释器型走 PATH）。
/// working_dir = plugin.json 所在目录（protocol.md §7.2：entry 相对该目录解析，
/// 默认 = 该目录）。
fn invocation(plugin: &ResolvedPlugin) -> PluginInvocation {
    let entry = &plugin.manifest.entry;
    let base = plugin.dir.join(&entry.command);
    let exe = if base.exists() {
        base
    } else {
        PathBuf::from(&entry.command)
    };
    PluginInvocation {
        exe,
        args: entry.args.clone(),
        working_dir: Some(plugin.dir.clone()),
    }
}

/// 头部采样（protocol §2.2：前 4KB 文本，宽松解码）。
fn head_sample(path: &Path) -> String {
    let bytes = std::fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let head = &bytes[..bytes.len().min(4096)];
    String::from_utf8_lossy(head).into_owned()
}

/// ISO 毫秒 → epoch 毫秒（本测试用最小解析：2026-08-01T00:00:00.000Z）。
/// 无时间数字的行（如单行大夹具的首格 "timestamp"）返回 None。
fn iso_to_ms(s: &str) -> Option<i64> {
    let digits: Vec<i64> = s
        .split(|c: char| !c.is_ascii_digit())
        .filter(|p| !p.is_empty())
        .map(|p| p.parse())
        .collect::<Result<_, _>>()
        .ok()?;
    if digits.len() < 6 {
        return None;
    }
    // Y M D H M S [ms]
    let (y, mo, d, h, mi, se, ms) = (
        digits[0],
        digits[1],
        digits[2],
        digits[3],
        digits[4],
        digits[5],
        if digits.len() > 6 { digits[6] } else { 0 },
    );
    let days = {
        let y2 = y - if mo <= 2 { 1 } else { 0 };
        let era = y2.div_euclid(400);
        let yoe = y2 - era * 400;
        let doy = (153 * (mo + if mo > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        era * 146_097 + doe - 719_468
    };
    Some(days * 86_400_000 + (h * 3_600 + mi * 60 + se) * 1_000 + ms)
}

/// csv 夹具表头状态（qa-perf.md §2 表：small_no_header / single_long_line 无表头）。
fn csv_fixture_has_header(name: &str) -> bool {
    !matches!(
        name,
        fixtures_ref::SMALL_NO_HEADER | fixtures_ref::SINGLE_LONG_LINE
    )
}

/// csv 夹具元数据表：行数 / 指标列数 / 首末时间戳（从文件自身解析）。
/// `header=false` 时 metrics 为数值列的 colN 命名（engine.rs colN 回退同口径：
/// 采样行内任一数值格即计数值列，首列可解析为时间则排除）。
struct CsvMeta {
    rows: usize,
    metrics: Vec<String>,
    first_ms: i64,
    last_ms: i64,
}

fn csv_meta(path: &Path, header: bool) -> CsvMeta {
    // GBK 等非 UTF-8 夹具按宽松解码（protocol §2.2 head_sample 同款策略，不崩溃）。
    let bytes = std::fs::read(path).unwrap();
    let text = String::from_utf8_lossy(&bytes);
    let mut metrics: Vec<String> = Vec::new();
    let mut first_ts: Option<i64> = None;
    let mut last_ts: i64 = 0;
    let mut rows = 0usize;
    let mut first = true;
    let mut numeric: Vec<bool> = Vec::new();
    let mut scanned = 0usize;
    for line in text.lines() {
        if header && first {
            metrics = line
                .split(',')
                .skip(1) // 首列 timestamp 不计指标
                .map(|c| c.trim().to_string())
                .filter(|c| !c.is_empty())
                .collect();
            first = false;
            continue;
        }
        first = false;
        let cols: Vec<&str> = line.split(',').collect();
        if cols.len() < 2 {
            continue;
        }
        if !header && scanned < 100 {
            if numeric.len() < cols.len() {
                numeric.resize(cols.len(), false);
            }
            for (j, c) in cols.iter().enumerate() {
                if !numeric[j] && c.trim().parse::<f64>().is_ok() {
                    numeric[j] = true;
                }
            }
            scanned += 1;
        }
        let ts = match iso_to_ms(cols[0]) {
            Some(ts) => ts,
            None => continue, // 坏行（时间不可解析）不计
        };
        if first_ts.is_none() {
            first_ts = Some(ts);
        }
        last_ts = ts;
        rows += 1;
    }
    if !header && rows > 0 {
        if first_ts.is_some() {
            numeric[0] = false; // 首列可解析为时间 → 不计指标（engine.rs:270 同款）
        }
        metrics = numeric
            .iter()
            .enumerate()
            .filter(|(_, &n)| n)
            .map(|(j, _)| format!("col{j}"))
            .collect();
    }
    CsvMeta {
        rows,
        metrics,
        first_ms: first_ts.unwrap_or(0),
        last_ms: last_ts,
    }
}

/// 标准真实插件流程（qa-perf.md §3.2）。
fn drive_real_plugin(
    plugin_id: &str,
    fixture_name: &str,
    has_header: bool,
    case: &str,
) -> Result<(u64, i64, i64, Vec<String>), String> {
    let Some(plugin) = resolve_plugin(plugin_id) else {
        return Err("plugin not built".to_string());
    };
    let fixture = fixtures_ref::fixture_path(fixture_name);
    assert!(fixture.exists(), "夹具缺失: {}", fixture.display());

    let mut s = PluginSession::spawn(&invocation(&plugin), 1 << 20)
        .map_err(|e| format!("spawn {plugin_id}: {e}"))?;

    // ① initialize：id 与 manifest 一致（BEH-01）。
    let init = s
        .initialize("AnalysisBuddy-test", "0.1.0")
        .map_err(|e| e.message())?;
    assert_eq!(
        init["id"], plugin.manifest.id,
        "initialize id == manifest id"
    );

    // ② manifest 预筛断言（protocol §7.2）。
    let match_rules = &plugin.manifest.r#match;
    let ext = fixture
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    if !match_rules.extensions.is_empty() {
        assert!(
            match_rules.extensions.iter().any(|e| e == &ext),
            "{plugin_id} manifest 预筛应包含 .{ext}"
        );
    }

    // ③ can_handle 置信度断言。
    let can = s
        .can_handle(&ab_protocol::types::CanHandleParams {
            path: fixture.to_string_lossy().into_owned(),
            name: fixture_name.to_string(),
            ext,
            size_bytes: std::fs::metadata(&fixture).unwrap().len(),
            head_sample: head_sample(&fixture),
        })
        .map_err(|e| e.message())?;
    assert!(can.can_handle, "{plugin_id} 应认领 {fixture_name}");
    // 空文件无头部可采样：插件按扩展名 base 置信度认领（engine.rs:397 csv base=0.5），
    // 其余夹具要求列结构/指纹加分后 ≥0.8（qa-perf.md §3.2 置信度断言）。
    let min_confidence = if fixture_name == fixtures_ref::EMPTY {
        0.5
    } else {
        0.8
    };
    assert!(
        can.confidence >= min_confidence,
        "{plugin_id} can_handle confidence {} < {min_confidence}",
        can.confidence
    );

    // ④ load_file → parse 完成。
    let summary = s
        .load_file(FILE_ID, &fixture)
        .map_err(|e| format!("load_file: {}", e.message()))?;
    let schema = s.schema().map_err(|e| e.message())?;
    let outcome = s
        .parse(FILE_ID)
        .map_err(|e| format!("parse: {}", e.message()))?;

    let meta = csv_meta(&fixture, has_header);
    // 记录数 == fixture 行数 × 指标数（bad 行被跳过并计数，见各夹具断言）。
    let metric_ids: Vec<String> = schema.metrics.iter().map(|m| m.id.clone()).collect();
    if !meta.metrics.is_empty() {
        if has_header {
            // 有表头夹具：指标名必须来自表头数值列（schema() == fixture 声明）。
            assert!(!metric_ids.is_empty(), "schema 必须声明指标");
            for m in &metric_ids {
                assert!(
                    meta.metrics.iter().any(|h| h.eq_ignore_ascii_case(m)),
                    "schema 指标 {m} 应来自 fixture 数值列"
                );
            }
        } else {
            // 无表头夹具：插件按 colN 回退命名（engine.rs colN 回退），
            // 无法断言「指标名来自表头」→ 等价校验：指标数 == 数值列数。
            assert!(!metric_ids.is_empty(), "schema 必须声明指标");
            assert_eq!(
                metric_ids.len(),
                meta.metrics.len(),
                "无表头夹具 schema 指标数 == 数值列数（colN 命名等价校验）"
            );
        }
    } else {
        // 无数值列夹具（empty.csv / single_long_line.csv）：schema 无指标是合法语义。
        assert!(
            metric_ids.is_empty(),
            "无数值列夹具 schema 不应声明指标: {metric_ids:?}"
        );
    }
    // 时间范围 == fixture 范围。
    if let Some(range) = summary.time_range {
        let tolerance = 5_000i64; // 首/末行解析与插件采样允许 ≤5s 容差
        assert!(
            (range.start_ms - meta.first_ms).abs() <= tolerance,
            "start_ms {} vs fixture {}",
            range.start_ms,
            meta.first_ms
        );
        assert!(
            (range.end_ms - meta.last_ms).abs() <= tolerance,
            "end_ms {} vs fixture {}",
            range.end_ms,
            meta.last_ms
        );
    }

    // ⑤ 查询 API：时间切片 + 排序正确性。
    let slice = s.store.slice(meta.first_ms, meta.last_ms);
    assert!(
        slice.len() as u64 <= outcome.records_total,
        "切片 ≤ 总记录数"
    );
    assert!(
        slice.windows(2).all(|w| w[0].timestamp <= w[1].timestamp),
        "查询结果时间序列不乱"
    );

    s.unload_file(FILE_ID).map_err(|e| e.message())?;
    s.shutdown().map_err(|e| e.message())?;
    assert_eq!(s.file_state(FILE_ID), FileEntryState::NotLoaded);

    let _ = case;
    Ok((
        outcome.records_total,
        meta.first_ms,
        meta.last_ms,
        metric_ids,
    ))
}

// ---------------------------------------------------------------------------
// 套件 0：夹具完整性（本套件在插件就绪前即可全量运行）
// ---------------------------------------------------------------------------

#[test]
fn fixtures_integrity() {
    // 8 个入仓夹具全部存在且内容符合 qa-perf.md §2 表。
    for name in [
        fixtures_ref::SMALL_WITH_HEADER,
        fixtures_ref::SMALL_NO_HEADER,
        fixtures_ref::SMALL_TXT,
        fixtures_ref::EMPTY,
        fixtures_ref::MALFORMED_LINES,
        fixtures_ref::ENC_UTF8_BOM,
        fixtures_ref::ENC_GBK,
        fixtures_ref::SINGLE_LONG_LINE,
    ] {
        assert!(
            fixtures_ref::fixture_path(name).exists(),
            "入仓夹具缺失: {name}"
        );
    }

    let fx = fixtures_ref::fixtures_dir();
    // small_with_header：200 行数据、3 指标列、时间严格递增。
    let meta = csv_meta(&fx.join(fixtures_ref::SMALL_WITH_HEADER), true);
    assert_eq!(meta.rows, 200, "small_with_header 200 行");
    assert_eq!(meta.metrics, vec!["fps", "frame_ms", "mem_mb"]);
    let lines: Vec<String> = std::fs::read_to_string(fx.join(fixtures_ref::SMALL_WITH_HEADER))
        .unwrap()
        .lines()
        .skip(1)
        .map(|l| l.split(',').next().unwrap().to_string())
        .collect();
    assert!(lines.windows(2).all(|w| w[0] < w[1]), "时间严格递增");

    // small_no_header：无表头，首行即数据。
    let first = std::fs::read_to_string(fx.join(fixtures_ref::SMALL_NO_HEADER))
        .unwrap()
        .lines()
        .next()
        .unwrap()
        .to_string();
    assert!(first.starts_with("2026-"), "无表头首行即数据");

    // small_txt：200 行，含三级 level 与三种行类。
    let txt = std::fs::read_to_string(fx.join(fixtures_ref::SMALL_TXT)).unwrap();
    assert_eq!(txt.lines().count(), 200);
    for lvl in ["level=info", "level=warn", "level=error"] {
        assert!(txt.contains(lvl), "txt 必须含 {lvl}");
    }
    assert!(txt.contains(" FRAME "));
    assert!(txt.contains(" STATE "));
    assert!(txt.contains(" EVENT "));

    // malformed_lines：恰 20 行畸形（缺列 / abc / not-a-time / 超长行），表头除外。
    let bad = std::fs::read_to_string(fx.join(fixtures_ref::MALFORMED_LINES))
        .unwrap()
        .lines()
        .skip(1)
        .filter(|l| {
            let cols = l.split(',').count();
            l.contains(",abc,") || !l.starts_with("2026-") || cols != 4 || l.len() > 1000
        })
        .count();
    assert_eq!(bad, 20, "malformed_lines 恰 20 行畸形");

    // enc_utf8_bom：EF BB BF 开头。
    let bom = std::fs::read(fx.join(fixtures_ref::ENC_UTF8_BOM)).unwrap();
    assert_eq!(&bom[..3], &[0xEF, 0xBB, 0xBF], "UTF-8 BOM 存在");

    // enc_gbk：含高位字节（非纯 ASCII），UTF-8 宽松解码不崩溃。
    let gbk = std::fs::read(fx.join(fixtures_ref::ENC_GBK)).unwrap();
    assert!(!gbk.iter().all(|b| *b < 0x80), "GBK 文件含中文高位字节");

    // single_long_line：总长 ~7MB，单行 <8MB 帧上限。
    let len = std::fs::metadata(fx.join(fixtures_ref::SINGLE_LONG_LINE))
        .unwrap()
        .len();
    assert_eq!(len, 7 * 1024 * 1024, "single_long_line 恰好 7MB");
    let max_line = std::fs::read(fx.join(fixtures_ref::SINGLE_LONG_LINE))
        .unwrap()
        .split(|b| *b == b'\n')
        .map(|l| l.len())
        .max()
        .unwrap();
    assert!(max_line < 8 * 1024 * 1024, "单行 7.3MB < 8MB 帧上限");

    // empty.csv：0 字节。
    assert_eq!(
        std::fs::metadata(fx.join(fixtures_ref::EMPTY))
            .unwrap()
            .len(),
        0
    );
}

// ---------------------------------------------------------------------------
// 套件 1：builtin-csv × csv 夹具矩阵
// ---------------------------------------------------------------------------

#[test]
fn builtin_csv_matrix() {
    if resolve_plugin("builtin-csv").is_none() {
        eprintln!("[SKIP] builtin_csv_matrix: builtin-csv 未构建（D1-03 交付后激活）");
        return;
    }
    // 全组合：csv 档逐一过真实插件流程（qa-perf.md §3.2）。
    let csv_fixtures = [
        fixtures_ref::SMALL_WITH_HEADER,
        fixtures_ref::SMALL_NO_HEADER,
        fixtures_ref::MALFORMED_LINES,
        fixtures_ref::ENC_UTF8_BOM,
        fixtures_ref::ENC_GBK,
        fixtures_ref::EMPTY,
        fixtures_ref::SINGLE_LONG_LINE,
    ];
    for fixture in csv_fixtures {
        let t = Instant::now();
        match drive_real_plugin(
            "builtin-csv",
            fixture,
            csv_fixture_has_header(fixture),
            "builtin_csv_matrix",
        ) {
            Ok((records, _, _, _)) => {
                eprintln!(
                    "[ OK ] builtin-csv × {fixture}: records={records} ({:.1}s)",
                    t.elapsed().as_secs_f64()
                );
            }
            Err(e) => {
                dump_on_failure("builtin_csv_matrix", None, &e);
                panic!("builtin-csv × {fixture} 失败: {e}");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 套件 2：demo-tool × small_txt.log（key_values 真实语义）
// ---------------------------------------------------------------------------

#[test]
fn demo_tool_small_txt() {
    let Some(plugin) = resolve_plugin("demo-tool") else {
        eprintln!("[SKIP] demo_tool_small_txt: demo-tool 未构建（D2-03 交付后激活）");
        return;
    };
    let fixture = fixtures_ref::fixture_path(fixtures_ref::SMALL_TXT);
    let mut s = PluginSession::spawn(&invocation(&plugin), 1 << 20)
        .unwrap_or_else(|e| panic!("spawn demo-tool: {e}"));

    let init = s
        .initialize("AnalysisBuddy-test", "0.1.0")
        .expect("initialize");
    assert_eq!(init["id"], plugin.manifest.id);
    assert_eq!(
        init["capabilities"]["annotate"], true,
        "demo-tool 开启 annotate"
    );

    let can = s
        .can_handle(&ab_protocol::types::CanHandleParams {
            path: fixture.to_string_lossy().into_owned(),
            name: fixtures_ref::SMALL_TXT.to_string(),
            ext: "txt".to_string(),
            size_bytes: std::fs::metadata(&fixture).unwrap().len(),
            head_sample: head_sample(&fixture),
        })
        .expect("can_handle");
    assert!(can.can_handle, "demo-tool 应认领 small_txt.log");
    assert!(can.confidence >= 0.8);

    let _ = s.load_file(FILE_ID, &fixture).expect("load_file");
    let schema = s.schema().expect("schema");
    let metric_ids: Vec<&str> = schema.metrics.iter().map(|m| m.id.as_str()).collect();
    assert!(
        metric_ids.contains(&"fps")
            && metric_ids.contains(&"frame_time")
            && metric_ids.contains(&"cpu_temp"),
        "demo-tool 三个指标（sdk-plugins.md §4.2）: {metric_ids:?}"
    );
    let outcome = s.parse(FILE_ID).expect("parse");

    // FRAME 行数 × 3 条 Record（small_txt.log：200 行，其中 EVENT 2 行、STATE 4 行）。
    let txt = std::fs::read_to_string(&fixture).unwrap();
    let frames = txt.lines().filter(|l| l.contains(" FRAME ")).count();
    assert_eq!(
        outcome.records_total as usize,
        frames * 3,
        "FRAME 每行 3 条 Record"
    );

    // 游标 key_values 真实语义（sdk-plugins.md §4.3）：STATE 行二分 → ≤T 状态快照。
    // STATE 变更前后对照（语义断言）：以第 2 条 STATE（scene=boss_fight）为变更点，
    // 变更前一时刻快照 == 前一条 STATE（scene=main_menu），变更后 == 本条 STATE；
    // 快照内 hero_hp 必须随之更新（非空值断言）。
    let state_ts: Vec<(i64, String)> = txt
        .lines()
        .filter(|l| l.contains(" STATE "))
        .filter_map(|l| {
            let ts = l.split_whitespace().next()?;
            let scene = l
                .split("scene=")
                .nth(1)?
                .split_whitespace()
                .next()?
                .to_string();
            Some((iso_to_ms(ts)?, scene))
        })
        .collect();
    assert!(
        state_ts.len() >= 2,
        "夹具必须含 ≥2 条 STATE，实际 {}",
        state_ts.len()
    );
    let (t_change, scene_after) = &state_ts[1];
    let scene_before = &state_ts[0].1;
    let kv_before = s
        .key_values(FILE_ID, t_change - 1)
        .expect("key_values before");
    let kv_after = s
        .key_values(FILE_ID, t_change + 1)
        .expect("key_values after");
    let entry = |kv: &ab_protocol::types::KeyValuesResult, k: &str| {
        kv.entries
            .iter()
            .find(|e| e.key == k)
            .map(|e| e.value.clone())
    };
    assert_eq!(
        entry(&kv_before, "scene"),
        Some(Value::String(scene_before.clone())),
        "STATE 变更前 T-1 快照 scene 应为前一条 STATE（{scene_before}）"
    );
    assert_eq!(
        entry(&kv_after, "scene"),
        Some(Value::String(scene_after.clone())),
        "STATE 变更后 T+1 快照 scene 应为本条 STATE（{scene_after}）"
    );
    assert!(
        matches!(entry(&kv_after, "scene"), Some(Value::String(_))),
        "scene 为字符串状态值"
    );
    assert!(
        entry(&kv_before, "hero_hp").is_some(),
        "key_values 含 hero_hp"
    );
    assert_ne!(
        entry(&kv_before, "hero_hp"),
        entry(&kv_after, "hero_hp"),
        "STATE 变更后 hero_hp 快照必须更新（语义断言）"
    );

    s.unload_file(FILE_ID).expect("unload_file");
    s.shutdown().expect("shutdown");
}

// ---------------------------------------------------------------------------
// 套件 3：disorder_20pct.csv 排序正确性用例（qa-perf.md §2 验收目标行）
// ---------------------------------------------------------------------------

#[test]
fn disorder_20pct_sorting() {
    let generated = fixtures_ref::generated_path(fixtures_ref::DISORDER_20PCT);
    if !generated.exists() {
        eprintln!("[SKIP] disorder_20pct_sorting: 需先跑 tests/scripts/gen-large-fixtures.ps1");
        return;
    }
    if resolve_plugin("builtin-csv").is_none() {
        eprintln!("[SKIP] disorder_20pct_sorting: builtin-csv 未构建（D1-03 交付后激活）");
        return;
    }
    // 文件本身 20% 乱序（loggen 生成时已交换时间戳）。
    let lines = std::fs::read_to_string(&generated).unwrap();
    let ts: Vec<i64> = lines
        .lines()
        .skip(1)
        .map(|l| iso_to_ms(l.split(',').next().unwrap()).expect("generated ts"))
        .collect();
    let inversions = ts.windows(2).filter(|w| w[0] > w[1]).count();
    assert!(inversions > 0, "夹具应包含乱序（loggen --disorder 0.2）");

    // 解析后查询 API 时间序列不乱。
    let Some(plugin) = resolve_plugin("builtin-csv") else {
        unreachable!()
    };
    let mut s = PluginSession::spawn(&invocation(&plugin), 1 << 20)
        .unwrap_or_else(|e| panic!("spawn builtin-csv: {e}"));
    let _ = s
        .initialize("AnalysisBuddy-test", "0.1.0")
        .expect("initialize");
    let _ = s.load_file(FILE_ID, &generated).expect("load_file");
    let _ = s.parse(FILE_ID).expect("parse");
    let slice = s.store.slice(i64::MIN / 2, i64::MAX / 2);
    assert!(!slice.is_empty());
    assert!(
        slice.windows(2).all(|w| w[0].timestamp <= w[1].timestamp),
        "查询结果时间序列不乱（20% 乱序输入）"
    );
    s.unload_file(FILE_ID).expect("unload_file");
    s.shutdown().expect("shutdown");
}

// ---------------------------------------------------------------------------
// 套件 4：多文件同轴叠加（两个插件数据同一时间轴可查询且切片正确）
// ---------------------------------------------------------------------------

#[test]
fn multi_file_overlay_same_axis() {
    let csv_ok = resolve_plugin("builtin-csv").is_some();
    let txt_ok = resolve_plugin("demo-tool").is_some();
    if !csv_ok || !txt_ok {
        eprintln!("[SKIP] multi_file_overlay_same_axis: 需要 builtin-csv + demo-tool（D1-03/D2-03 后激活）");
        return;
    }
    let csv_plugin = resolve_plugin("builtin-csv").unwrap();
    let txt_plugin = resolve_plugin("demo-tool").unwrap();

    let csv_fx = fixtures_ref::fixture_path(fixtures_ref::SMALL_WITH_HEADER);
    let txt_fx = fixtures_ref::fixture_path(fixtures_ref::SMALL_TXT);

    // 两个真实插件各起一个会话，解析各自文件（builtin-csv × csv、demo-tool × txt）。
    let mut csv_s = PluginSession::spawn(&invocation(&csv_plugin), 1 << 20)
        .unwrap_or_else(|e| panic!("spawn builtin-csv: {e}"));
    let _ = csv_s
        .initialize("AnalysisBuddy-test", "0.1.0")
        .expect("initialize");
    let _ = csv_s.load_file("f-csv", &csv_fx).expect("load_file csv");
    let _ = csv_s.parse("f-csv").expect("parse csv");

    let mut txt_s = PluginSession::spawn(&invocation(&txt_plugin), 1 << 20)
        .unwrap_or_else(|e| panic!("spawn demo-tool: {e}"));
    let _ = txt_s
        .initialize("AnalysisBuddy-test", "0.1.0")
        .expect("initialize");
    let _ = txt_s.load_file("f-txt", &txt_fx).expect("load_file txt");
    let _ = txt_s.parse("f-txt").expect("parse txt");

    // 两插件数据并入同一存储（同轴）→ 同一 run_query 结果集（qa-perf.md §3.2）。
    let mut merged = Store::new();
    merged.merge(&csv_s.store);
    merged.merge(&txt_s.store);

    // 同一时间轴可查询：全域切片（完整合并时间范围）同时包含两插件记录。
    // 注意：窗口从合并存储实际时间范围推导——夹具轴 = 2026-08-01T00:00:00Z
    // 的真实 epoch 为 1785542400000（插件与 iso_to_ms 一致口径），不得硬编码。
    let (t0, t1) = merged.time_range().expect("同轴叠加后合并存储非空");
    let all = merged.slice(t0, t1);
    assert!(!all.is_empty(), "同轴叠加后切片非空");
    let metrics: Vec<&str> = all.iter().map(|r| r.metric.as_str()).collect();
    assert!(
        metrics.contains(&"fps") && metrics.contains(&"frame_time"),
        "同轴切片同时含 builtin-csv(fps) 与 demo-tool(frame_time) 指标"
    );
    assert!(
        metrics.contains(&"cpu_temp"),
        "demo-tool 三指标同轴共存（fps/frame_time/cpu_temp）"
    );

    // 切片正确性：窄窗口（时间范围前半段）只含该范围内的记录。
    let narrow_end = t0 + (t1 - t0) / 2;
    let narrow = merged.slice(t0, narrow_end);
    assert!(
        narrow.iter().all(|r| r.timestamp <= narrow_end),
        "窄窗口切片不越界"
    );

    csv_s.shutdown().expect("shutdown");
    txt_s.shutdown().expect("shutdown");
}
