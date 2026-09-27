//! Fuzz target ①：帧层 —— 随机字节流 → [`ab_host::rpc::FrameReader`]
//! （8 MB 长度上限 + UTF-8/JSON 校验 + run_read_loop 同构分流）。
//!
//! 被测实现与不变量见 `../../src/lib.rs` 的 `fuzz_frame_read`。

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    ab_fuzz_harness::fuzz_frame_read(data);
});
