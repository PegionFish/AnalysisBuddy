//! A-02 帧边界测试（host-runtime.md §4.1/§4.2 DoD）：
//! 恰好 8MB 行通过、超 8MB 行在整行读满前返回 LineTooLong、孤立 `\r`、
//! 非法 JSON、EOF 与残留半行；id 单调递增。

use std::io::Cursor;

use ab_host::{FrameError, FrameReader, RpcChannel};
use serde_json::json;
use tokio::sync::mpsc;

fn reader(bytes: Vec<u8>) -> FrameReader<Cursor<Vec<u8>>> {
    FrameReader::new(Cursor::new(bytes))
}

#[tokio::test]
async fn exactly_8mb_line_passes_the_length_gate() {
    let max = FrameReader::<Cursor<Vec<u8>>>::MAX_LINE_BYTES;
    // 合法 JSON 单行，总长恰好 8,388,608 字节。
    let base = r#"{"jsonrpc":"2.0","id":1,"result":""#;
    let tail = r#""}"#;
    let pad = max - base.len() - tail.len();
    assert!(pad > 0, "test shape sanity: pad = {pad}");
    let mut line = String::with_capacity(max + 1);
    line.push_str(base);
    line.extend(std::iter::repeat('a').take(pad));
    line.push_str(tail);
    assert_eq!(
        line.len(),
        max,
        "line length must be exactly MAX_LINE_BYTES"
    );

    let mut bytes = line.into_bytes();
    bytes.push(b'\n');
    let mut fr = reader(bytes);
    let value = fr.next_frame().await.expect("8MB line must pass the gate");
    assert_eq!(value["id"], json!(1));
    assert_eq!(
        value["result"].as_str().map(|s| s.len()),
        Some(pad),
        "payload round-trips"
    );
}

#[tokio::test]
async fn over_8mb_line_returns_line_too_long_before_line_is_complete() {
    let max = FrameReader::<Cursor<Vec<u8>>>::MAX_LINE_BYTES;
    // 8MB + 1 字节且整行尚未终结（无换行）——必须返回 LineTooLong。
    let bytes: Vec<u8> = std::iter::repeat(b'a').take(max + 1).collect();
    let mut fr = reader(bytes);
    let err = fr.next_frame().await.expect_err("over-limit line rejected");
    assert_eq!(err, FrameError::LineTooLong);
}

#[tokio::test]
async fn stray_cr_marks_malformed_line() {
    let mut bytes = br#"{"jsonrpc":"2.0","id":1,"result":{}}"#.to_vec();
    bytes.extend_from_slice(b"\r\n");
    let mut fr = reader(bytes);
    let err = fr.next_frame().await.expect_err("stray \\r rejected");
    assert_eq!(err, FrameError::MalformedLine);
}

#[tokio::test]
async fn invalid_json_returns_invalid_json() {
    let mut fr = reader(b"this is not json\n".to_vec());
    let err = fr.next_frame().await.expect_err("non-JSON rejected");
    assert_eq!(err, FrameError::InvalidJson);
}

#[tokio::test]
async fn eof_on_empty_stream() {
    let mut fr = reader(Vec::new());
    assert_eq!(fr.next_frame().await, Err(FrameError::Eof));
}

#[tokio::test]
async fn partial_trailing_line_at_eof_is_malformed() {
    let mut fr = reader(br#"{"jsonrpc":"2.0","id":1"#.to_vec());
    let err = fr
        .next_frame()
        .await
        .expect_err("half line at EOF rejected");
    assert_eq!(err, FrameError::MalformedLine);
}

#[tokio::test]
async fn empty_line_is_malformed() {
    let mut fr = reader(b"\n".to_vec());
    assert_eq!(fr.next_frame().await, Err(FrameError::MalformedLine));
}

#[tokio::test]
async fn multiple_frames_parsed_in_sequence() {
    let buf = concat!(
        r#"{"jsonrpc":"2.0","id":1,"result":{"a":1}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","method":"progress","params":{"file_id":"f","records_so_far":0}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32003,"message":"x"}}"#,
        "\n",
    );
    let mut fr = reader(buf.as_bytes().to_vec());
    let f1 = fr.next_frame().await.expect("frame 1");
    assert_eq!(f1["id"], json!(1));
    let f2 = fr.next_frame().await.expect("frame 2");
    assert_eq!(f2["method"], "progress");
    let f3 = fr.next_frame().await.expect("frame 3");
    assert_eq!(f3["error"]["code"], json!(-32003));
    assert_eq!(fr.next_frame().await, Err(FrameError::Eof));
}

#[tokio::test]
async fn ids_are_monotonic_from_one() {
    let (tx, mut frames_rx) = mpsc::channel::<String>(8);
    let chan = RpcChannel::new(tx);
    let mut seen = Vec::new();
    // 无读泵：调用必然超时，但写侧帧与 id 分配不受影响。
    for _ in 0..3 {
        let outcome = chan
            .call("ping", json!({}), std::time::Duration::from_millis(100))
            .await
            .expect("channel alive");
        assert!(
            matches!(outcome, ab_host::RpcOutcome::TransportError(_)),
            "no reader: every call times out into a transport outcome"
        );
        let f = frames_rx.recv().await.expect("frame written");
        let id = serde_json::from_str::<serde_json::Value>(&f).unwrap()["id"]
            .as_u64()
            .expect("numeric id");
        seen.push(id);
    }
    assert_eq!(seen, [1, 2, 3], "ids are monotonically increasing from 1");
}
