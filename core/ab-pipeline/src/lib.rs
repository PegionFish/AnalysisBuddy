//! AnalysisBuddy 数据管线（B 路）：导入编排、解析调度、内存存储、查询 API、
//! 会话文件。实现依据 `pipeline.md`（AnalysisBuddy-devdocs/deep-dive/）。
//!
//! 对 A 路（ab-host）的依赖以 trait 形式声明（pipeline.md §4.1 适配层约定），
//! Phase 2 落地；本卡仅为可编译占位，不含任何业务逻辑。

/// 验证 test harness 可用的占位单测。
#[test]
fn test_harness_works() {
    assert_eq!(3 * 3, 9);
}
