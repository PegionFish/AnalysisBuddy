//! 运行时 ACL 回归（任务 15 根因固化）：直接构造**与真实打包版同款**的
//! `tauri::ipc::RuntimeAuthority`（`tauri::runtime_authority!` 宏路径，
//! 即 `Webview::on_message` → `resolve_access` 实际调用的同一函数），
//! 输入为 build.rs/tauri-build 落盘的真实解析原料：
//!
//! - `gen/schemas/acl-manifests.json`：app 命令权限（`allow-*`，build.rs
//!   `AppManifest::commands` 生成）+ core/dialog 插件权限清单；
//! - `gen/schemas/capabilities.json`：capabilities/default.json 的解析产物。
//!
//! 经 `tauri_utils::acl::resolved::Resolved::resolve`（与 codegen 同一条
//! 解析路径、同一 Target::Windows）展开 `core:default`/`dialog:default`
//! 集合为叶子命令后，逐条断言 `resolve_access`：
//!
//! 1. `list_plugins` / `plugin:dialog|open` / `plugin:dialog|save` /
//!    `plugin:event|listen` 在 `main` 窗口必须放行（任务 15 缺陷 1/2/3）；
//! 2. 未授权命令（`plugin:dialog|close`）必须被拒——证明 ACL 真实在场；
//! 3. 故障态复现：同权限但 `windows: []` 的合成 capability 必须全拒
//!    （固化任务 15 根因：resolve_access 对空 windows glob 列表 `any()`
//!    恒 false，缺 windows 字段 = 全部 IPC 被拒）。
//!
//! 不构建 App/Window：**实测**本机测试宿主上任何含 `tauri::App` 构建
//! （mock_builder().build / mock_app）的测试二进制在 loader 阶段即崩溃，
//! 错误为 `0xc0000139 STATUS_ENTRYPOINT_NOT_FOUND`（"The procedure entry
//! point TaskDialogIndirect could not be located in the dynamic link library
//! comctl32.dll"）——本机 `comctl32.dll` 缺导出 `TaskDialogIndirect`，而
//! App 构建路径经 muda（common-controls-v6 菜单子系统）静态链接该符号；
//! 与 WebView2 无关（MockRuntime 纯内存实现，无 WebView2 入口点；崩溃先于
//! 任何测试代码运行，--list 即崩）。故只测权限判定本身——这正是运行时
//! 拒绝发生的唯一判定点。

use std::collections::BTreeMap;

use tauri::ipc::Origin;
use tauri_utils::acl::capability::Capability;
use tauri_utils::acl::manifest::Manifest;
use tauri_utils::acl::resolved::Resolved;
use tauri_utils::platform::Target;

/// build.rs（tauri-build）落盘的 ACL 清单：插件权限 manifest + app 权限。
fn load_acl_manifests() -> BTreeMap<String, Manifest> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/gen/schemas/acl-manifests.json"
    );
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("读取 acl-manifests.json 失败（先跑一次 build.rs）：{e}"));
    serde_json::from_str(&raw).expect("acl-manifests.json 反序列化失败")
}

/// capabilities/default.json 的解析产物（tauri-build 生成，含 windows 字段）。
fn load_capabilities() -> BTreeMap<String, Capability> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/gen/schemas/capabilities.json");
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("读取 capabilities.json 失败（先跑一次 build.rs）：{e}"));
    serde_json::from_str(&raw).expect("capabilities.json 反序列化失败")
}

/// 用真实 gen 产物解析并构造运行时权限裁决器（与 codegen/生产同一代码路径）。
fn build_authority(capabilities: BTreeMap<String, Capability>) -> tauri::ipc::RuntimeAuthority {
    let acl = load_acl_manifests();
    let resolved = Resolved::resolve(&acl, capabilities, Target::Windows)
        .expect("capabilities 解析失败——检查标识符写法与权限清单是否在场");
    // 与生产二进制同一构造宏：debug 下带原始 acl，release 下仅 Resolved。
    tauri::runtime_authority!(acl, resolved)
}

/// 真实 capability（gen 产物原样）构造的裁决器。
fn real_authority() -> tauri::ipc::RuntimeAuthority {
    build_authority(load_capabilities())
}

/// main 窗口（config 中唯一窗口标签）+ 本地 origin 的权限判定。
fn access(authority: &tauri::ipc::RuntimeAuthority, command: &str) -> bool {
    authority
        .resolve_access(command, "main", "main", &Origin::Local)
        .is_some()
}

/// 缺陷 1 回归：`list_plugins` 必须在 main 窗口被 ACL 放行。
/// 修复前（capability 无 windows 字段）resolve_access 恒 None，
/// 前端收到 "Command list_plugins not allowed by ACL"，插件页恒"暂无插件"。
#[test]
fn list_plugins_passes_acl_on_main_window() {
    let authority = real_authority();
    assert!(
        access(&authority, "list_plugins"),
        "list_plugins 被 ACL 拒绝——capability 必须声明 windows:[\"main\"]（任务 15 缺陷 1）"
    );
}

/// 缺陷 2 回归：dialog 权限集合 `dialog:default` 必须已展开为叶子命令
/// `plugin:dialog|open` / `plugin:dialog|save` 并放行。
/// （回应二轮取证"二进制找不到 dialog:default 字面串"：集合标识符在
/// Resolved::resolve 阶段即展开，release 只嵌展开产物，此处断言展开在场。）
#[test]
fn dialog_default_expands_to_open_and_save_grants() {
    let authority = real_authority();
    assert!(
        access(&authority, "plugin:dialog|open"),
        "plugin:dialog|open 未获 ACL 放行——dialog:default 集合展开缺失或被丢弃（任务 15 缺陷 2）"
    );
    assert!(
        access(&authority, "plugin:dialog|save"),
        "plugin:dialog|save 未获 ACL 放行——保存会话对话框必拒（任务 15 缺陷 2）"
    );
}

/// 缺陷 3 回归：`plugin:event|listen` 必须放行——前端 listen() 与
/// OS 拖放事件（tauri://drag-enter/drop）订阅全依赖它，被拒则拖放全哑。
#[test]
fn event_listen_passes_acl_on_main_window() {
    let authority = real_authority();
    assert!(
        access(&authority, "plugin:event|listen"),
        "plugin:event|listen 被 ACL 拒绝——前端 listen()/OS 拖放事件全哑（任务 15 缺陷 3，core:default 展开需含 core:event）"
    );
}

/// 对照防线：未授权命令必须被拒——证明上面的放行不是"检查被整体绕过"。
#[test]
fn ungranted_command_is_denied_by_acl() {
    let authority = real_authority();
    assert!(
        !access(&authority, "plugin:dialog|close"),
        "未授权命令 plugin:dialog|close 竟被放行——ACL 判定失效"
    );
}

/// C2.1（P0-02）：`cancel_parse` 必须在 main 窗口被 ACL 放行——缺失则前端
/// 取消按钮 invoke 被静默拒绝，P0-02 修复不可达。
#[test]
fn cancel_parse_passes_acl_on_main_window() {
    let authority = real_authority();
    assert!(
        access(&authority, "cancel_parse"),
        "cancel_parse 被 ACL 拒绝——capability 必须声明 allow-cancel-parse（C2.1）"
    );
}

/// 故障态复现（任务 15 根因固化）：与真实 capability 相同权限、
/// 但 `windows: []` 的合成 capability，resolve_access 必须全部返回 None。
/// 这就是打包版三个缺陷的共同根因：空 windows 模式列表 → glob `any()`
/// 恒 false → 所有 invoke/listen 被拒。任何人再删掉 capability 的
/// windows 字段，此测试不会挂，但 capabilities_test 的覆盖断言会挂；
/// 本测试固化的是运行时语义本身。
#[test]
fn capability_without_windows_denies_everything() {
    let synthetic: BTreeMap<String, Capability> = [(
        "synthetic-no-windows".to_string(),
        serde_json::from_value(serde_json::json!({
            "identifier": "synthetic-no-windows",
            "description": "任务 15 故障态复现：权限齐全但缺 windows 字段",
            "permissions": [
                "core:default",
                "dialog:default",
                "allow-list-plugins"
            ]
        }))
        .expect("合成 capability 反序列化失败"),
    )]
    .into_iter()
    .collect();

    let authority = build_authority(synthetic);
    for command in ["list_plugins", "plugin:dialog|open", "plugin:event|listen"] {
        assert!(
            !access(&authority, command),
            "windows=[] 时 {command} 竟被放行——resolve_access 语义已变化，重新评估任务 15 结论"
        );
    }
}
