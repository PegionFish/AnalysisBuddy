//! 回归防线（任务 12 根因固化）：Tauri 2 ACL capabilities 缺失曾导致打包版
//! 全部 invoke 被静默拒绝（插件页恒"暂无插件" + 保存对话框无响应）。
//!
//! 本测试做三方交叉校验，任一侧漂移即失败：
//! 1. 前端命令清单（`ui/src/ipc/real.ts` 的 `call('xxx')`）；
//! 2. 生产命令注册集（`src/lib.rs` `generate_handler!` 宏展开清单）；
//! 3. ACL 授权（`capabilities/default.json` 的 `allow-*` 权限，权限清单由
//!    `build.rs` `REGISTERED_COMMANDS` 经 tauri-build 自动生成）。
//!
//! 另断言事件监听（`core:event`）与保存对话框（`dialog`）权限在场——
//! 这两者同样走 ACL，缺失时症状与 command 被拒一致（静默失败）。

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

/// 仓库根（CARGO_MANIFEST_DIR = core/ab-app）。
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("core dir")
        .parent()
        .expect("repo root")
        .to_path_buf()
}

/// 从 `ui/src/ipc/real.ts` 提取前端 invoke 命令名（`call('xxx'` 形状）。
fn frontend_commands() -> BTreeSet<String> {
    let src = fs::read_to_string(repo_root().join("ui/src/ipc/real.ts"))
        .expect("read ui/src/ipc/real.ts");
    let mut commands = BTreeSet::new();
    let mut cursor = 0;
    while let Some(offset) = src[cursor..].find("call('") {
        let start = cursor + offset + "call('".len();
        let end = src[start..].find('\'').expect("unterminated call('...')") + start;
        commands.insert(src[start..end].to_string());
        cursor = end;
    }
    assert!(
        !commands.is_empty(),
        "real.ts 未解析出任何 invoke 命令——解析规则失效"
    );
    commands
}

/// 从 `src/lib.rs` 提取 `tauri::generate_handler![...]` 注册的命令名
/// （`commands::mod::name,` 行的末段）。
fn handler_commands() -> BTreeSet<String> {
    let src =
        fs::read_to_string(repo_root().join("core/ab-app/src/lib.rs")).expect("read lib.rs");
    let start = src
        .find("generate_handler![")
        .expect("lib.rs 缺少 generate_handler!")
        + "generate_handler![".len();
    let end = src[start..].find(']').expect("generate_handler! 未闭合") + start;
    let mut commands = BTreeSet::new();
    for line in src[start..end].lines() {
        let line = line.trim().trim_end_matches(',');
        if let Some(path) = line.strip_prefix("commands::") {
            let name = path.rsplit("::").next().expect("command path");
            commands.insert(name.to_string());
        }
    }
    assert!(
        !commands.is_empty(),
        "generate_handler! 未解析出任何命令——解析规则失效"
    );
    commands
}

/// 从 `build.rs` 提取 `REGISTERED_COMMANDS` 清单（capabilities 权限生成源）。
fn build_script_commands() -> BTreeSet<String> {
    let src =
        fs::read_to_string(repo_root().join("core/ab-app/build.rs")).expect("read build.rs");
    let const_pos = src
        .find("REGISTERED_COMMANDS")
        .expect("build.rs 缺少 REGISTERED_COMMANDS");
    let start = src[const_pos..]
        .find("= &[")
        .expect("REGISTERED_COMMANDS 数组初始化")
        + const_pos
        + "= &[".len();
    let end = src[start..].find(']').expect("REGISTERED_COMMANDS 未闭合") + start;
    let mut commands = BTreeSet::new();
    let mut cursor = start;
    while let Some(offset) = src[cursor..end].find('"') {
        let item_start = cursor + offset + 1;
        let item_end = src[item_start..end]
            .find('"')
            .expect("REGISTERED_COMMANDS 字符串未闭合")
            + item_start;
        commands.insert(src[item_start..item_end].to_string());
        cursor = item_end + 1;
    }
    assert!(
        !commands.is_empty(),
        "REGISTERED_COMMANDS 未解析出任何命令——解析规则失效"
    );
    commands
}

/// `capabilities/default.json` 的 permissions 数组。
fn capability_permissions() -> Vec<String> {
    let path = repo_root().join("core/ab-app/capabilities/default.json");
    let raw = fs::read_to_string(&path).expect(
        "缺少 capabilities/default.json——Tauri 2 会静默拒绝全部 invoke（任务 12 根因）",
    );
    let value: serde_json::Value = serde_json::from_str(&raw).expect("capabilities JSON 非法");
    value["permissions"]
        .as_array()
        .expect("capabilities 缺少 permissions 数组")
        .iter()
        .map(|p| p.as_str().expect("permission 须为字符串").to_string())
        .collect()
}

/// 命令名 → tauri-build 生成的 allow 权限标识符（下划线转连字符，
/// 见 tauri-utils `autogenerate_command_permissions`）。
fn allow_permission(command: &str) -> String {
    format!("allow-{}", command.replace('_', "-"))
}

/// 防线 1：生产命令注册集（lib.rs）与前端调用清单（real.ts）逐字等价。
#[test]
fn handler_registration_matches_frontend_commands() {
    let frontend = frontend_commands();
    let handler = handler_commands();
    let unregistered: Vec<_> = frontend.difference(&handler).collect();
    assert!(
        unregistered.is_empty(),
        "前端调用但 invoke_handler 未注册（release 必挂）：{unregistered:?}"
    );
    let unreferenced: Vec<_> = handler.difference(&frontend).collect();
    assert!(
        unreferenced.is_empty(),
        "invoke_handler 注册但前端未引用（死注册，需同步 real.ts 或清理）：{unreferenced:?}"
    );
}

/// 防线 2：build.rs REGISTERED_COMMANDS 与 lib.rs 注册集同步——
/// 它是 ACL 权限自动生成源，漂移即 capabilities 校验在构建期爆炸。
#[test]
fn build_script_command_list_matches_handler() {
    let build_script = build_script_commands();
    let handler = handler_commands();
    assert_eq!(
        build_script, handler,
        "build.rs REGISTERED_COMMANDS 与 generate_handler! 不一致：\nbuild.rs={build_script:?}\nlib.rs={handler:?}"
    );
}

/// 防线 3：capabilities 逐命令授权 + core:event（listen）+ dialog（保存对话框）。
#[test]
fn capabilities_cover_every_invoked_command() {
    let permissions = capability_permissions();
    let frontend = frontend_commands();
    for command in &frontend {
        let required = allow_permission(command);
        assert!(
            permissions.iter().any(|p| p == &required),
            "capabilities/default.json 缺少 `{required}`（command `{command}` 将被 ACL 静默拒绝——任务 12 症状）"
        );
    }
}

#[test]
fn capabilities_grant_event_listen_and_dialog() {
    let permissions = capability_permissions();
    assert!(
        permissions.iter().any(|p| p == "core:default" || p == "core:event:default"),
        "缺少 core:default/core:event:default——前端 listen() 将被 ACL 拒绝，ab://* 事件全哑"
    );
    assert!(
        permissions
            .iter()
            .any(|p| p == "dialog:default" || p == "dialog:allow-save"),
        "缺少 dialog:default/dialog:allow-save——save_session 另存为对话框将被 ACL 拒绝"
    );
}

/// 防线 4：capabilities/default.json 不得为空能力（{} 或空 permissions 等价于
/// 任务 12 故障态）。
#[test]
fn capabilities_not_empty() {
    let permissions = capability_permissions();
    assert!(
        !permissions.is_empty(),
        "capabilities permissions 为空——所有 invoke 将被静默拒绝（任务 12 故障态）"
    );
}

/// `capabilities/default.json` 解析后的完整 JSON。
fn capability_value() -> serde_json::Value {
    let path = repo_root().join("core/ab-app/capabilities/default.json");
    let raw = fs::read_to_string(&path).expect("read capabilities/default.json");
    serde_json::from_str(&raw).expect("capabilities JSON 非法")
}

/// `tauri.conf.json` 声明的窗口标签（无 label 时 Tauri 默认 "main"）。
fn configured_window_labels() -> Vec<String> {
    let raw = fs::read_to_string(repo_root().join("core/ab-app/tauri.conf.json"))
        .expect("read tauri.conf.json");
    let conf: serde_json::Value = serde_json::from_str(&raw).expect("tauri.conf.json 非法");
    let windows = conf["app"]["windows"]
        .as_array()
        .expect("tauri.conf.json 缺少 app.windows");
    windows
        .iter()
        .map(|w| {
            w["label"]
                .as_str()
                .unwrap_or("main")
                .to_string()
        })
        .collect()
}

/// 防线 5（任务 15 根因）：capability 必须声明非空 windows（或 webviews）。
/// Tauri 2 运行时 `RuntimeAuthority::resolve_access` 要求 windows/webviews
/// glob 命中当前窗口标签（`Vec::iter().any()`）——空列表对任何窗口都不匹配，
/// 等价于全部 invoke/listen 被拒。缺 windows 字段是打包版二轮复验失败的直接原因。
#[test]
fn capability_windows_cover_every_configured_window() {
    let cap = capability_value();
    let windows: Vec<String> = cap["windows"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|v| v.as_str().expect("windows 项须为字符串").to_string())
                .collect()
        })
        .unwrap_or_default();
    let webviews: Vec<String> = cap["webviews"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .map(|v| v.as_str().expect("webviews 项须为字符串").to_string())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        !windows.is_empty() || !webviews.is_empty(),
        "capability 未声明 windows/webviews——运行时 resolve_access 永不匹配，全部 IPC 被拒（任务 15 根因）"
    );
    // 静态 glob 覆盖检查：精确同名或通配 "*" 视为覆盖。
    let covers = |label: &str| {
        windows.iter().any(|p| p == label || p == "*")
            || webviews.iter().any(|p| p == label || p == "*")
    };
    for label in configured_window_labels() {
        assert!(
            covers(&label),
            "capability windows/webviews 未覆盖 tauri.conf.json 窗口 `{label}`——该窗口的全部 IPC 将被拒"
        );
    }
}

/// 防线 6（任务 15 缺陷 3）：OS 拖放不得被配置禁用。Tauri 2 语义：
/// `dragDropEnabled` 默认 true（wry 注册 OLE drop 目标并发 tauri://drag-* 事件，
/// 前端 FilePanel 依赖）；显式 false 时 OS 拖放完全失效。
#[test]
fn drag_drop_enabled_not_disabled() {
    let raw = fs::read_to_string(repo_root().join("core/ab-app/tauri.conf.json"))
        .expect("read tauri.conf.json");
    let conf: serde_json::Value = serde_json::from_str(&raw).expect("tauri.conf.json 非法");
    for window in conf["app"]["windows"]
        .as_array()
        .expect("tauri.conf.json 缺少 app.windows")
    {
        for key in ["dragDropEnabled", "drag_drop_enabled", "drag-drop-enabled"] {
            if let Some(value) = window.get(key) {
                assert_eq!(
                    value,
                    &serde_json::Value::Bool(true),
                    "窗口 `{key}` 被置为 false——OS 文件拖放失效，前端 tauri://drag-* 监听永不触发（任务 15 缺陷 3）"
                );
            }
        }
    }
}
