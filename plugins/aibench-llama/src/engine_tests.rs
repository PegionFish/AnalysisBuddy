//! engine 模块单元测试（从 engine.rs 拆出）。

use super::*;
use std::sync::Arc;

const HEADER: &str = "sn,machine_model,cpu,cpu_vendor,gpu,gpu_vendor,driver_version,vbios,bios_version,ec_version,os_version,os_arch,total_ram_gb,engine_id,backend,llama_build,model_id,task_name,n_prompt,n_gen,ngl,flash_attn,avg_ts,stddev_ts,ttft_ms_measured,ttft_ms_est,vram_peak_mb,vram_mode,load_time_ms,test_time,samples,ppl,avg_power_w,energy_per_token_mj,rounds,turns_ok,tool_time_total_s,e2e_duration_s,session_success,p95_tps,degradation_ratio,suite_name,host_version,timestamp,status";
const SUITE: &str = "stress48h-qwen35-4b,1.0.0+abc,2026-09-04 18:34:02,ok";

/// 构造一行 45 列夹具（逗号直拼，不含引号）。
#[allow(clippy::too_many_arguments)]
fn row(
    backend: &str,
    task: &str,
    n_prompt: &str,
    n_gen: &str,
    avg_ts: &str,
    ttft: &str,
    ppl: &str,
    _ts: &str,
) -> String {
    format!(
        "SN001,X6AF,Intel CPU,GenuineIntel,NVIDIA GPU,NVIDIA,drv,vbios,bios,ec,Win11,x64,31.5,cuda-13.3-x64,{backend},10621,qwen3.5-4b-q4km,{task},{n_prompt},{n_gen},99,-1,{avg_ts},1.5,{ttft},,7206,inline,2045,0.41,5,{ppl},113.08,18.19,,,,,,,,{suite}",
        suite = SUITE
    )
}

fn lf_from(content: &str) -> LoadedFile {
    load_content("f1", content).unwrap()
}

#[derive(Default)]
struct Rec {
    batches: Vec<RecordBatch>,
    progresses: Vec<(Option<f64>, u64, Option<u64>)>,
}
impl Sink for Rec {
    fn batch(&mut self, batch: RecordBatch) {
        self.batches.push(batch);
    }
    fn progress(&mut self, percent: Option<f64>, records_so_far: u64, bytes_read: Option<u64>) {
        self.progresses.push((percent, records_so_far, bytes_read));
    }
}

#[test]
fn row_fixture_self_check() {
    assert_eq!(HEADER.split(',').count(), 45);
    let r = row(
        "cuda",
        "probe-pp@cuda",
        "512",
        "0",
        "6213.7",
        "",
        "",
        "2026-09-04 18:34:02",
    );
    assert_eq!(r.split(',').count(), 45);
}

#[test]
fn load_rejects_non_aibench_csv() {
    assert!(load_content("f1", "a,b,c\n1,2,3\n").is_err());
    assert!(load_content("f1", "timestamp,fps\n2026-08-07T00:00:00Z,60\n").is_err());
}

#[test]
fn load_and_parse_full_flow() {
    let content = format!(
        "{HEADER}\n{}\n{}\n{}\n{}\n",
        row(
            "cuda-13.3-x64",
            "probe-pp@cuda-13.3-x64",
            "512",
            "0",
            "6213.7",
            "",
            "",
            "2026-09-04 18:34:02"
        ),
        row(
            "cuda-13.3-x64",
            "probe-tg@cuda-13.3-x64",
            "0",
            "512",
            "154.0",
            "",
            "",
            "2026-09-04 18:34:02"
        ),
        row(
            "cuda-13.3-x64",
            "probe-ppl@cuda-13.3-x64",
            "",
            "",
            "",
            "",
            "7.403",
            "2026-09-04 18:36:10"
        ),
        row(
            "vulkan-x64",
            "probe-pp@vulkan-x64",
            "512",
            "0",
            "5655.7",
            "",
            "",
            "2026-09-04 18:34:13"
        ),
    );
    let mut lf = lf_from(&content);
    assert_eq!(lf.rows.len(), 4);
    assert_eq!(lf.bad_lines, 0);
    assert_eq!(lf.suite_name, "stress48h-qwen35-4b");
    let tr = lf.time_range.unwrap();
    // 时间戳来自 SUITE 常量（18:34:02）；4 行同秒 → 去重后 end = start + 3ms。
    assert_eq!(tr.start_ms, parse_ts("2026-09-04 18:34:02").unwrap());
    assert_eq!(tr.end_ms, parse_ts("2026-09-04 18:34:02").unwrap() + 3);

    let mut rec = Rec::default();
    let cancel = Arc::new(AtomicBool::new(false));
    let total = parse_file(&mut lf, &mut rec, &cancel).unwrap();
    // 每行基线非空指标：stddev, vram, load, power, energy = 5
    // pp/tg 行 +avg_ts = 6；ppl 行 +ppl = 6；合计 6×4 = 24
    assert_eq!(total, 24, "records_total");
    let sum: usize = rec.batches.iter().map(|b| b.records.len()).sum();
    assert_eq!(sum as u64, total);
    assert!(rec.batches.last().unwrap().done);
    let seqs: Vec<u64> = rec.batches.iter().map(|b| b.seq).collect();
    assert_eq!(seqs, (0..seqs.len() as u64).collect::<Vec<_>>());

    let r0 = &rec.batches[0].records[0];
    assert_eq!(r0.metric, "avg_ts");
    assert!((r0.value - 6213.7).abs() < 1e-9);
    let tags = r0.tags.as_ref().unwrap();
    assert_eq!(tags.get("engine_id"), Some(&"cuda-13.3-x64".to_string()));
    assert_eq!(tags.get("backend"), Some(&"cuda-13.3-x64".to_string()));
    assert_eq!(tags.get("model_id"), Some(&"qwen3.5-4b-q4km".to_string()));
    // tags 为原始列值（task_name 保留 @backend 后缀；归一仅用于 key_values 分组）。
    assert_eq!(
        tags.get("task_name"),
        Some(&"probe-pp@cuda-13.3-x64".to_string())
    );

    // 同一秒多行 → 按行序 +1ms 去重（row() 的时间戳参数为文档性；SUITE 内烘焙 18:34:02）。
    let ts0 = parse_ts("2026-09-04 18:34:02").unwrap();
    let ts_rows: Vec<i64> = rec
        .batches
        .iter()
        .flat_map(|b| b.records.iter())
        .filter(|r| r.metric == "avg_ts")
        .map(|r| r.timestamp)
        .collect();
    assert_eq!(ts_rows[0], ts0);
    assert_eq!(ts_rows[1], ts0 + 1, "同一秒第二行 +1ms");
    assert_eq!(
        ts_rows[2],
        ts0 + 3,
        "第3个 avg_ts 属第4行（ppl 行无 avg_ts 但占用 +2ms）"
    );
}

#[test]
fn tag_names_map_to_columns() {
    assert_eq!(
        TAG_COLUMNS,
        &["engine_id", "backend", "model_id", "task_name"]
    );
}

#[test]
fn bad_rows_counted() {
    let content = format!(
        "{HEADER}\n{}\n{}\n",
        row(
            "cuda",
            "probe-pp@cuda",
            "512",
            "0",
            "6213.7",
            "",
            "",
            "2026-09-04 18:34:02"
        ),
        "SN001,too,few,columns",
    );
    let mut lf = lf_from(&content);
    assert_eq!(lf.bad_lines, 1);
    assert!(lf.note.contains("skipped 1 bad lines"));
    assert!(lf.bad_samples[0].1.contains("column count"));
    let mut rec = Rec::default();
    let cancel = Arc::new(AtomicBool::new(false));
    let total = parse_file(&mut lf, &mut rec, &cancel).unwrap();
    assert_eq!(total, 6);
}

#[test]
fn status_not_ok_counted_bad() {
    let content = format!(
        "{HEADER}\n{}\n",
        row(
            "cuda",
            "probe-pp@cuda",
            "512",
            "0",
            "6213.7",
            "",
            "",
            "2026-09-04 18:34:02"
        )
    );
    let bad_status = content.replacen(",ok", ",failed", 1);
    let lf = lf_from(&bad_status);
    assert_eq!(lf.bad_lines, 1);
    assert!(lf.bad_samples[0].1.contains("status"));
}

#[test]
fn key_values_latest_per_task() {
    let content = format!(
        "{HEADER}\n{}\n{}\n",
        row(
            "cuda",
            "soak48h-r01@cuda",
            "512",
            "0",
            "6060.5",
            "",
            "",
            "2026-09-04 18:38:39"
        ),
        row(
            "cuda",
            "soak48h-r02@cuda",
            "512",
            "0",
            "6126.6",
            "",
            "",
            "2026-09-04 18:39:55"
        ),
    );
    // timestamp 烘焙在 SUITE 常量中（18:34:02），两行同秒 → 第二行 +1ms 去重。
    let lf = lf_from(&content);
    let t = parse_ts("2026-09-04 18:35:00").unwrap();
    let r = key_values(&lf, t);
    assert!(r.entries.iter().any(|e| e.key == "suite_name"));
    let r01: Vec<_> = r
        .entries
        .iter()
        .filter(|e| e.key.starts_with("task:soak48h-r01:"))
        .collect();
    assert!(r01.iter().any(|e| e.key == "task:soak48h-r01:avg_ts"));
    let avg01 = r01.iter().find(|e| e.key.ends_with("avg_ts")).unwrap();
    assert!((avg01.value.as_f64().unwrap() - 6060.5).abs() < 1e-9);
    assert!(r.entries.iter().any(|e| e.key == "task:soak48h-r02:avg_ts"));
    // T 早于行完成时刻（18:34:02）→ 无任务条目。
    let r_early = key_values(&lf, parse_ts("2026-09-04 18:34:01").unwrap());
    assert!(!r_early.entries.iter().any(|e| e.key.starts_with("task:")));
}

#[test]
fn can_handle_claims_aibench_and_rejects_generic() {
    let params = |ext: &str, head: &str| CanHandleParams {
        path: "x".into(),
        name: format!("a.{ext}"),
        ext: ext.to_string(),
        size_bytes: 100,
        head_sample: head.to_string(),
    };
    let r = can_handle(&params("csv", HEADER));
    assert!(r.can_handle);
    assert!((r.confidence - 1.0).abs() < 1e-9, "{}", r.confidence);
    let r = can_handle(&params(
        "csv",
        "timestamp,fps,frame_ms\n2026-08-07T00:00:00Z,60,16\n",
    ));
    assert!(!r.can_handle && r.confidence == 0.0);
    let r = can_handle(&params("txt", HEADER));
    assert!(!r.can_handle && r.confidence == 0.0);
    let r = can_handle(&params("csv", "a,b\n1,2\n"));
    assert!(!r.can_handle);
}

#[test]
fn parse_cancellation() {
    let content = format!(
        "{HEADER}\n{}\n",
        row(
            "cuda",
            "probe-pp@cuda",
            "512",
            "0",
            "6213.7",
            "",
            "",
            "2026-09-04 18:34:02"
        )
    );
    let mut lf = lf_from(&content);
    let mut rec = Rec::default();
    let cancel = Arc::new(AtomicBool::new(true));
    assert!(matches!(
        parse_file(&mut lf, &mut rec, &cancel),
        Err(ParseError::Cancelled)
    ));
}

#[test]
fn summary_shape() {
    let content = format!(
        "{HEADER}\n{}\n",
        row(
            "cuda",
            "probe-pp@cuda",
            "512",
            "0",
            "6213.7",
            "",
            "",
            "2026-09-04 18:34:02"
        )
    );
    let lf = lf_from(&content);
    let s = to_summary(&lf);
    assert!(s.record_count_hint.is_some());
    assert!(s.time_range.is_some());
    assert!(s.note.unwrap().contains("AIBench schema"));
}
