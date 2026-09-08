//! NDJSON 帧层（protocol-v1.md §1.2/§1.3）：UTF-8 无 BOM、LF、8MB 先行校验、孤立 `\r` 拒绝。
//! 与 builtin-csv 的 ndjson 模块同构（独立实现，不依赖 SDK crate）。

use std::io::{self, BufRead, Write};

pub const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;

/// 逐行读帧：按字节增量读、长度先于内容校验（超 8MB 即中止，不驻留整行）。
pub struct FrameReader<R: BufRead> {
    inner: R,
    buf: Vec<u8>,
}

impl<R: BufRead> FrameReader<R> {
    pub fn new(inner: R) -> Self {
        FrameReader {
            inner,
            buf: Vec::with_capacity(4096),
        }
    }

    /// 返回下一帧文本；EOF 返回 None；帧层违规 Err。
    pub fn read_frame(&mut self) -> Result<Option<String>, String> {
        let mut chunk = [0u8; 8192];
        loop {
            if let Some(nl) = self.buf.iter().position(|&b| b == b'\n') {
                if nl > MAX_LINE_BYTES {
                    return Err("stdin line exceeds 8MB limit".to_string());
                }
                let line: Vec<u8> = self.buf.drain(..nl).collect();
                self.buf.drain(..1); // 换行
                return self.finalize(line);
            }
            if self.buf.len() > MAX_LINE_BYTES {
                return Err("stdin line exceeds 8MB limit".to_string());
            }
            let n = self
                .inner
                .read(&mut chunk)
                .map_err(|e| format!("stdin read failed: {e}"))?;
            if n == 0 {
                if self.buf.is_empty() {
                    return Ok(None);
                }
                if self.buf.len() > MAX_LINE_BYTES {
                    return Err("stdin line exceeds 8MB limit".to_string());
                }
                let line = std::mem::take(&mut self.buf);
                return self.finalize(line);
            }
            if !chunk[..n].contains(&b'\n') && self.buf.len() + n > MAX_LINE_BYTES {
                return Err("stdin line exceeds 8MB limit".to_string());
            }
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }

    fn finalize(&self, line: Vec<u8>) -> Result<Option<String>, String> {
        if line.last() == Some(&b'\r') {
            return Err("stray CR at frame boundary (CRLF line ending rejected)".to_string());
        }
        match String::from_utf8(line) {
            Ok(s) => Ok(Some(s)),
            Err(_) => Err("invalid UTF-8 on stdin".to_string()),
        }
    }
}

/// 整帧写 stdout：单行 JSON + `\n`，随后 flush（宿主按行增量读取）。
/// 调用方负责并发互斥（main.rs 的发送锁），保证整行原子写出。
pub fn write_frame(out: &mut impl Write, value: &serde_json::Value) -> io::Result<()> {
    let line = serde_json::to_string(value).map_err(io::Error::other)?;
    out.write_all(line.as_bytes())?;
    out.write_all(b"\n")?;
    out.flush()
}
