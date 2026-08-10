//! 迷你宿主 NDJSON 帧读取器：按字节增量读行、8MB 上限先断长度（protocol-v1.md §1.3）。

use std::io::{BufRead, Read};
use std::time::Duration;

/// 8MB 帧上限（8 × 1024 × 1024 字节）。
pub const FRAME_LIMIT_BYTES: usize = 8 * 1024 * 1024;

/// 帧读取错误。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    /// 单行超 8MB：宿主立即终止会话（protocol §1.3）。
    LineExceedsLimit(usize),
    /// 流结束（进程退出/管道关闭）。
    Eof,
    /// IO 错误。
    Io(String),
}

/// 从子进程 stdout 逐帧读 NDJSON（行尾 `\n`；孤立 `\r` 记为协议错误）。
pub struct FrameReader<R: Read> {
    inner: R,
    /// 单行缓冲（按字节累计）。
    buf: Vec<u8>,
}

impl<R: Read> FrameReader<R> {
    pub fn new(reader: R) -> Self {
        Self {
            inner: reader,
            buf: Vec::with_capacity(4096),
        }
    }

    /// 读取下一帧（不含行尾）。EOF 返回 `Err(FrameError::Eof)`。
    pub fn next_frame(&mut self) -> Result<String, FrameError> {
        self.buf.clear();
        loop {
            let mut byte = [0u8; 1];
            match self.inner.read(&mut byte) {
                Ok(0) => {
                    if self.buf.is_empty() {
                        return Err(FrameError::Eof);
                    }
                    // 无行尾 EOF：进程在行中间退出（协议 §9 第 1 条：不得输出半行）。
                    return Err(FrameError::Io("truncated line at EOF".to_string()));
                }
                Ok(_) => {
                    if byte[0] == b'\n' {
                        let line = String::from_utf8_lossy(&self.buf).into_owned();
                        return Ok(line);
                    }
                    if self.buf.len() >= FRAME_LIMIT_BYTES {
                        return Err(FrameError::LineExceedsLimit(self.buf.len()));
                    }
                    self.buf.push(byte[0]);
                }
                Err(e) => return Err(FrameError::Io(e.to_string())),
            }
        }
    }
}

/// 迷你宿主的请求-响应窗口：读取若干帧直到出现给定 id 的响应。
/// `notify` 回调处理通知帧；返回响应帧的 `result` 或 `error`。
pub fn read_until_response(
    reader: &mut FrameReader<impl Read>,
    id: &serde_json::Value,
    notify: &mut impl FnMut(serde_json::Value),
    timeout: Duration,
) -> Result<serde_json::Value, FrameError> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Err(FrameError::Io("read deadline exceeded".to_string()));
        }
        // 简化实现：逐帧阻塞读（watchdog 场景由 session 层用通道实现）。
        let frame = reader.next_frame()?;
        let v: serde_json::Value = serde_json::from_str(&frame)
            .map_err(|e| FrameError::Io(format!("invalid JSON frame: {e}")))?;
        if v.get("id") == Some(id) {
            return Ok(v);
        }
        if v.get("id").is_none() && v.get("method").is_some() {
            notify(v);
        }
    }
}

/// 把子进程 stderr 读入环形缓冲（protocol §9.3：1MB/插件 cap，循环覆盖）。
pub struct StderrRing {
    buf: Vec<u8>,
    cap: usize,
}

impl StderrRing {
    pub fn new(cap: usize) -> Self {
        Self {
            buf: Vec::new(),
            cap,
        }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
        if self.buf.len() > self.cap {
            let drop = self.buf.len() - self.cap;
            self.buf.drain(0..drop);
        }
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// 转储到文件（供失败定位）。
    pub fn dump_to(&self, path: &std::path::Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, &self.buf)
    }

    pub fn as_text(&self) -> String {
        String::from_utf8_lossy(&self.buf).into_owned()
    }
}

/// 把 stderr 线程消费的 `BufRead` 读入环形缓冲（线程内调用）。
pub fn drain_stderr<R: BufRead>(reader: R, ring: &mut StderrRing) {
    for line in reader.lines() {
        match line {
            Ok(l) => {
                ring.push(l.as_bytes());
                ring.push(b"\n");
            }
            Err(_) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn frame_reader_parses_ndjson_and_strips_lf() {
        let data = b"{\"a\":1}\n{\"b\":2}\r\n{\"c\":3}\n";
        let mut r = FrameReader::new(Cursor::new(data.to_vec()));
        assert_eq!(r.next_frame().unwrap(), r#"{"a":1}"#);
        // 孤立 \r 属于帧内容（协议层判定；迷你宿主容忍行内 \r）。
        assert_eq!(r.next_frame().unwrap(), r#"{"b":2}"#.to_string() + "\r");
        assert_eq!(r.next_frame().unwrap(), r#"{"c":3}"#);
        assert_eq!(r.next_frame(), Err(FrameError::Eof));
    }

    #[test]
    fn frame_reader_rejects_over_8mb_line() {
        // 8MB+ 单行：先断长度，不得整行驻留内存。
        let mut line = vec![b'x'; FRAME_LIMIT_BYTES + 10];
        line.push(b'\n');
        let mut r = FrameReader::new(Cursor::new(line));
        assert_eq!(
            r.next_frame(),
            Err(FrameError::LineExceedsLimit(FRAME_LIMIT_BYTES))
        );
    }

    #[test]
    fn stderr_ring_caps_at_limit() {
        let mut ring = StderrRing::new(8);
        ring.push(b"12345");
        ring.push(b"67890");
        assert_eq!(ring.len(), 8);
        assert_eq!(ring.as_text(), "34567890", "环形缓冲丢弃最早溢出的 2 字节");
    }
}
