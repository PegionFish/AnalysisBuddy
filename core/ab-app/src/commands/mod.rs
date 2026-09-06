//! 命令层 Tauri 包装器组（M1 起）：全部 `*_logic` 纯函数与 DTO 迁至
//! `ab_engine::commands`（headless 共用）；本 crate 各子模块仅保留
//! `#[tauri::command]` 薄包装 + 公共项再导出，`ab_app::commands::*` 公共
//! API 面不变（16 个集成测试零修改）。
//!
//! `IpcError` 为全部 command 的 reject 值形状；`key_values_at` 除外——它走
//! 部分失败协议（§1.6），逐文件错误进入结果项，整体永不 reject。
//! 错误映射唯一实现见 [`ab_engine::ipc_errors`]（§1.10 表）。

pub mod import;
pub mod plugin;
pub mod plugin_manager;
pub mod presets;
pub mod query;
pub mod session;

pub use ab_engine::commands::{
    import_result_to_dto, plugin_source_name, CapabilitiesDto, ChartViewStateDto, FileTimeRangeDto,
    ImportOverride, ImportResultDto, IpcError, LoadResultDto, MissingFileEntryDto, PluginInfoDto,
    PluginMatchDto, SessionMetaDto, SessionSnapshotDto, TimeRangeDto,
};
