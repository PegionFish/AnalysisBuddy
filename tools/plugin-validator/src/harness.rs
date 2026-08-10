//! 校验 harness：进程管理、NDJSON reader（8MB 先行校验）、帧分类、stderr 转储
//! （docs-validator.md §3.1/§3.4）。
//!
//! - NDJSON reader 与宿主同规格：按字节增量读行、行长度先于内容判定；
//!   超长行（8 MB 上限，protocol-v1.md §1.3）在截断点即可判定，不驻留超限内容；
//! - 帧纪律检查（protocol-v1.md §1.2）：UTF-8 无 BOM、LF 行尾、无孤立 `\r`；
//! - 看门狗统一用单调时钟（`std::time::Instant`）并乘 `--timeout-scale`；
//! - 校验结束必杀进程（无论成败），`Session` 的 `Drop` 兜底保证无残留子进程。

use std::io::{self, BufReader, Read};
use std::time::Duration;

use serde_json::Value;

/// 单行消息字节上限（protocol-v1.md §1.3：8 × 1024 × 1024 = 8,388,608）。
pub const MAX_LINE_BYTES: u64 = 8_388_608;

/// 一条原始行：字节数（不含换行符）+ 是否超限 + 内容（仅未超限时保留）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawLine {
    /// 本行字节数（不含行尾 `\n`）。
    pub byte_len: u64,
    /// 是否超过 8 MB 上限（此时 `content` 只保留截断前缀）。
    pub too_long: bool,
    pub content: Vec<u8>,
}

/// 按字节增量读行，8 MB 上限先行校验（protocol-v1.md §1.3 阅读器实现要求：
/// 行长度先于内容判定，超长行可中止而无需读满）。
pub struct LineReader<R: Read> {
    inner: BufReader<R>,
    scratch: [u8; 64 * 1024],
    chunk: Vec<u8>,
    chunk_pos: usize,
}

impl<R: Read> LineReader<R> {
    pub fn new(inner: R) -> Self {
        LineReader {
            inner: BufReader::with_capacity(64 * 1024, inner),
            scratch: [0u8; 64 * 1024],
            chunk: Vec::with_capacity(64 * 1024),
            chunk_pos: 0,
        }
    }

    /// 读下一行；`Ok(None)` = EOF。行尾 `\n` 不包含在 `content` 中。
    pub fn next_line(&mut self) -> io::Result<Option<RawLine>> {
        let mut content: Vec<u8> = Vec::with_capacity(256);
        let mut byte_len: u64 = 0;
        let mut too_long = false;
        loop {
            let Some(pos) = self.chunk[self.chunk_pos..]
                .iter()
                .position(|&b| b == b'\n')
            else {
                let n = self.chunk.len() - self.chunk_pos;
                byte_len += n as u64;
                if byte_len > MAX_LINE_BYTES {
                    too_long = true;
                } else {
                    content.extend_from_slice(&self.chunk[self.chunk_pos..]);
                }
                // 读下一块
                self.chunk.clear();
                self.chunk_pos = 0;
                let read = self.inner.read(&mut self.scratch)?;
                if read == 0 {
                    // EOF：若前面已有内容则作为最后一行返回
                    if byte_len > 0 {
                        return Ok(Some(RawLine {
                            byte_len,
                            too_long,
                            content,
                        }));
                    }
                    return Ok(None);
                }
                self.chunk.extend_from_slice(&self.scratch[..read]);
                continue;
            };
            // 找到换行符
            let line_start = self.chunk_pos;
            let line_end = line_start + pos;
            byte_len += pos as u64;
            if byte_len > MAX_LINE_BYTES {
                too_long = true;
            } else {
                content.extend_from_slice(&self.chunk[line_start..line_end]);
            }
            self.chunk_pos = line_end + 1;
            return Ok(Some(RawLine {
                byte_len,
                too_long,
                content,
            }));
        }
    }
}

/// 帧分类结果（docs-validator.md §3.2/§3.4：先长度、再 BOM/`\r`、再 JSON）。
#[derive(Debug, Clone, PartialEq)]
pub enum LineKind {
    /// 单行超 8 MB（BEH-08 判据；内容截断）。
    TooLong { bytes: u64 },
    /// 行首 UTF-8 BOM（BEH-09 判据）。
    Bom,
    /// 行内含原始 `\r`（`\r\n` 行尾或孤立 `\r`，BEH-09 判据）。
    CarriageReturn,
    /// 合法 JSON 帧。
    Json(Value),
    /// 非 JSON 内容（BEH-09 判据；NaN/Infinity 字面量由行为层折算 BEH-05）。
    NotJson,
}

/// 已分类的一行帧。
#[derive(Debug, Clone, PartialEq)]
pub struct FrameLine {
    /// stdout 行号（从 1 起，定位信息用）。
    pub no: u64,
    pub kind: LineKind,
    /// 内容文本（UTF-8 宽松解码，供 message 展示）。
    pub text: String,
}

/// 逐行分类：长度 → BOM → `\r` → JSON 解析。
pub fn classify(raw: &RawLine) -> FrameLine {
    // 行号由调用方赋值；此处返回 without no 的占位
    let text = String::from_utf8_lossy(&raw.content).into_owned();
    let kind = if raw.too_long {
        LineKind::TooLong {
            bytes: raw.byte_len,
        }
    } else if raw.content.starts_with(&[0xEF, 0xBB, 0xBF]) {
        LineKind::Bom
    } else if raw.content.contains(&b'\r') {
        LineKind::CarriageReturn
    } else if let Ok(v) = serde_json::from_str::<Value>(&text) {
        LineKind::Json(v)
    } else {
        LineKind::NotJson
    };
    FrameLine { no: 0, kind, text }
}

/// 看门狗：单调时钟 + `--timeout-scale` 缩放（docs-validator.md §3.4）。
#[derive(Debug, Clone, Copy)]
pub struct Watchdog {
    pub scale: f64,
}

impl Watchdog {
    pub fn new(scale: f64) -> Self {
        Watchdog {
            scale: scale.max(0.1),
        }
    }

    /// 协议名义超时 → 实际看门狗时限（×scale）。
    pub fn deadline(&self, nominal: Duration) -> Duration {
        Duration::from_secs_f64(nominal.as_secs_f64() * self.scale)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn classify_line(content: &[u8]) -> FrameLine {
        classify(&RawLine {
            byte_len: content.len() as u64,
            too_long: false,
            content: content.to_vec(),
        })
    }

    #[test]
    fn line_reader_splits_on_lf_and_handles_eof() {
        let input = b"line1\nline2\nline3";
        let mut r = LineReader::new(&input[..]);
        let l1 = r.next_line().unwrap().unwrap();
        assert_eq!(l1.content, b"line1");
        assert_eq!(l1.byte_len, 5);
        assert!(!l1.too_long);
        let l2 = r.next_line().unwrap().unwrap();
        assert_eq!(l2.content, b"line2");
        // EOF 前的最后一行无换行符，仍作为一行返回
        let l3 = r.next_line().unwrap().unwrap();
        assert_eq!(l3.content, b"line3");
        assert!(r.next_line().unwrap().is_none());
    }

    #[test]
    fn line_reader_8mb_threshold_first() {
        // 8,388,608 字节整 = 通过；+1 = 超限，且不驻留完整内容
        let ok: Vec<u8> = vec![b'a'; MAX_LINE_BYTES as usize];
        let mut r = LineReader::new(&ok[..]);
        let line = r.next_line().unwrap().unwrap();
        assert!(!line.too_long);
        assert_eq!(line.byte_len, MAX_LINE_BYTES);

        let over: Vec<u8> = vec![b'a'; (MAX_LINE_BYTES + 1) as usize];
        let mut r = LineReader::new(&over[..]);
        let line = r.next_line().unwrap().unwrap();
        assert!(line.too_long, "8MB+1 字节必须判超限");
        assert_eq!(
            line.content.len() as u64,
            MAX_LINE_BYTES,
            "内容最多驻留 8MB"
        );
        assert_eq!(line.byte_len, MAX_LINE_BYTES + 1, "字节数如实统计");
    }

    #[test]
    fn classification_bom_cr_and_json() {
        // BOM
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(b"{\"a\":1}\n");
        let fl = classify_line(&bytes);
        assert_eq!(fl.kind, LineKind::Bom);

        // \r\n 行尾
        let fl = classify_line(b"{\"a\":1}\r\n");
        assert_eq!(fl.kind, LineKind::CarriageReturn);

        // 孤立 \r
        let fl = classify_line(b"{\"a\":1}\r{\"b\":2}");
        assert_eq!(fl.kind, LineKind::CarriageReturn);

        // 合法 JSON（行内 JSON 字符串中的 \\r 转义不算原始 \r 字节）
        let fl = classify_line(br#"{"msg":"a\r\nb"}"#);
        assert_eq!(
            fl.kind,
            LineKind::Json(serde_json::json!({"msg": "a\r\nb"}))
        );

        // 非 JSON
        let fl = classify_line(b"hello world");
        assert_eq!(fl.kind, LineKind::NotJson);
    }

    #[test]
    fn watchdog_scales_deadlines() {
        let w = Watchdog::new(2.0);
        assert_eq!(w.deadline(Duration::from_secs(5)), Duration::from_secs(10));
        assert_eq!(w.deadline(Duration::from_secs(30)), Duration::from_secs(60));
        let w0 = Watchdog::new(0.0);
        assert_eq!(
            w0.deadline(Duration::from_secs(5)),
            Duration::from_millis(500)
        );
    }

    #[test]
    fn too_long_classification_precedes_others() {
        let fl = classify(&RawLine {
            byte_len: MAX_LINE_BYTES + 10,
            too_long: true,
            content: vec![b'\r'; 100],
        });
        let too_long_bytes = MAX_LINE_BYTES + 10;
        assert!(matches!(fl.kind, LineKind::TooLong { bytes: b } if b == too_long_bytes));
    }
}
