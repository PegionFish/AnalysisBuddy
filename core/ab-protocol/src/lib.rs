//! AnalysisBuddy Plugin Protocol 共享类型——契约唯一事实来源。
//!
//! 设计正本见 `protocol.md`（AnalysisBuddy-devdocs/deep-dive/），Phase 1
//! 契约冻结（tag `contract-v1`）后同步至 `docs/spec/protocol-v1.md`。
//! 本 crate 仅承载类型定义，不含任何业务逻辑。

/// 验证 test harness 可用的占位单测。
#[test]
fn test_harness_works() {
    assert_eq!(2 + 2, 4);
}
