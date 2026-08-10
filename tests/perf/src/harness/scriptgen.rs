//! mock 流式剧本生成：把合成 Record 按契约类型序列化成 mock-plugin 的 NDJSON 剧本
//! （RecordBatch 通知流 + parse 应答）。`sleep_ms` 拉长批次间隔（RSS 采样窗口）。

use std::io::Write;
use std::path::Path;

use ab_protocol::types::{ParseResult, Record, RecordBatch};

/// 生成剧本到 `out`。`records` 总数按 `batch_size` 分批；`sleep_ms` = 批间睡眠。
pub fn gen_mock_script(
    records: u64,
    batch_size: usize,
    sleep_ms: u64,
    seed: u64,
    out: &Path,
) -> Result<(), String> {
    let file_id = "f-perf";
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("create dir {}: {e}", parent.display()))?;
    }
    let mut w = std::io::BufWriter::with_capacity(
        256 * 1024,
        std::fs::File::create(out).map_err(|e| format!("create {}: {e}", out.display()))?,
    );

    let init = r#"{"kind":"reply","method":"initialize","result":{"id":"mock","name":"Mock Replay Plugin","version":"0.1.0","capabilities":{"annotate":false,"subscribe":false,"binary_sidecar":false}}}"#;
    w.write_all(init.as_bytes()).map_err(|e| e.to_string())?;
    w.write_all(b"\n").map_err(|e| e.to_string())?;

    let load = format!(
        r#"{{"kind":"reply","method":"load_file","result":{{"record_count_hint":{records}}}}}"#
    );
    w.write_all(load.as_bytes()).map_err(|e| e.to_string())?;
    w.write_all(b"\n").map_err(|e| e.to_string())?;

    // parse 块：逐批 emit（含批间 sleep），最后 reply records_total。
    let batches = records.div_ceil(batch_size as u64);
    let mut rng = seed;
    for seq in 0..batches {
        let start = seq * batch_size as u64;
        let end = (start + batch_size as u64).min(records);
        let batch = RecordBatch {
            file_id: file_id.to_string(),
            seq,
            records: (start..end)
                .map(|i| {
                    rng = rng
                        .wrapping_mul(6364136223846793005)
                        .wrapping_add(1442695040888963407);
                    let ts = 1_785_542_400_000 + (i * 100) as i64;
                    let v = (rng % 1200) as f64 / 10.0;
                    Record {
                        timestamp: ts,
                        metric: "fps".to_string(),
                        value: v,
                        level: None,
                        tags: None,
                        raw_line: None,
                    }
                })
                .collect(),
            done: seq + 1 == batches,
        };
        let params = serde_json::to_string(&batch).map_err(|e| e.to_string())?;
        let line = format!(r#"{{"kind":"emit","method":"RecordBatch","params":{params}}}"#);
        w.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
        w.write_all(b"\n").map_err(|e| e.to_string())?;
        if sleep_ms > 0 && seq + 1 < batches {
            let s = format!(r#"{{"kind":"sleep","ms":{sleep_ms}}}"#);
            w.write_all(s.as_bytes()).map_err(|e| e.to_string())?;
            w.write_all(b"\n").map_err(|e| e.to_string())?;
        }
    }
    let pr = ParseResult {
        records_total: records,
    };
    let reply = format!(
        r#"{{"kind":"reply","method":"parse","result":{}}}"#,
        serde_json::to_string(&pr).map_err(|e| e.to_string())?
    );
    w.write_all(reply.as_bytes()).map_err(|e| e.to_string())?;
    w.write_all(b"\n").map_err(|e| e.to_string())?;

    let unload = r#"{"kind":"reply","method":"unload_file","result":{}}"#;
    w.write_all(unload.as_bytes()).map_err(|e| e.to_string())?;
    w.write_all(b"\n").map_err(|e| e.to_string())?;
    let shutdown = r#"{"kind":"reply","method":"shutdown","result":{}}"#;
    w.write_all(shutdown.as_bytes())
        .map_err(|e| e.to_string())?;
    w.write_all(b"\n").map_err(|e| e.to_string())?;
    w.flush().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_sizes_and_content() {
        let dir = std::env::temp_dir();
        let path = dir.join("ab-perf-scriptgen-test.ndjson");
        gen_mock_script(10_000, 1000, 0, 7, &path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            text.lines().count(),
            2 + 10 + 1 + 2,
            "init+load+10 batches+reply+unload+shutdown"
        );
        assert!(text.contains(r#""seq":9"#));
        assert!(text.contains(r#""done":true"#));
        assert!(text.contains(r#""records_total":10000"#));
        let _ = std::fs::remove_file(&path);
    }
}
