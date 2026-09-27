//! Fuzz target ②：JSON-RPC 入站类型 —— 随机字节 / 随机 JSON 值 →
//! ab-protocol 全部入站类型（19 个）的反序列化矩阵。
//!
//! 覆盖形态：截断 / 类型错位 / 深嵌套（serde_json 128 层递归上限断言）/
//! 超大数（1e999） / 重复键 / 非法 UTF-8 / NaN·±∞ 位模式。
//! 被测实现与不变量见 `../../src/lib.rs` 的 `fuzz_rpc_types`。

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    ab_fuzz_harness::fuzz_rpc_types(data);
});
