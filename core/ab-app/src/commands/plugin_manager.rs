//! 模块管理命令（spec §4.1/§4.2/§4.4/§5.1/§5.2）：`install_plugin_zip`、
//! `uninstall_plugin`、`set_plugin_enabled`（T6 追加 check/update）。
//!
//! 安装管线（§4.2 七步）：① 限额（≤100MB、≤2000 条目）→ ② zip-slip 防护
//! （`enclosed_name` 拒绝绝对路径/`..` 越界）→ ③ 解压到 plugins/ 下临时
//! 目录 → ④ 根 plugin.json 解析 + 宿主校验（与发现扫描同函数）→
//! ⑤ 冲突判定（内建拒绝/同版本已安装/不同版本需 overwrite）→
//! ⑥ 原子搬入 plugins/<id>/（先删旧再 rename）→ ⑦ registry.reload()。
//!
//! 卸载（§4.4）：关闭该插件全部文件会话 → 终止插件进程
//! （`shutdown_plugin_sessions`，live 进程 CWD 句柄会阻塞删目录）→ 删目录
//! → reload；内建拒绝。
//! 禁用（§4.4 + §3.2）：写状态文件 `.ab-modules.json` → `set_disabled`。
//!
//! 全部命令统一 `rename_all = "snake_case"`（任务 21：tauri-macros 默认
//! camelCase，与前端 snake_case 契约不符时参数静默失配）。

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ab_host::PluginRegistry;
use zip::ZipArchive;

use crate::commands::{IpcError, PluginInfoDto};
use crate::ipc_errors::module_error;
use crate::pipeline_bridge::ImportCoordinator;

/// ZIP 大小上限（spec §5.2：≤100MB）。
const ZIP_MAX_BYTES: u64 = 100 * 1024 * 1024;
/// ZIP 条目数上限（spec §5.2：≤2000）。
const ZIP_MAX_ENTRIES: usize = 2000;
/// 单条目解压尺寸上限（膨胀防护：声明 uncompressed size > 500MiB 拒绝，
/// 判定发生在写盘前，按中心目录 `entry.size()`）。
const UNPACKED_ENTRY_MAX_BYTES: u64 = 500 * 1024 * 1024;
/// 全 ZIP 累计解压尺寸上限（膨胀防护：1GiB，逐条目累积）。
const TOTAL_UNPACKED_MAX_BYTES: u64 = 1024 * 1024 * 1024;
/// 模块状态文件名（spec §3.2）。
const MODULE_STATE_FILE: &str = ".ab-modules.json";

/// 生产 plugins 目录：与 `PluginRegistry::new()` 的 Portable 源同一公式
/// （宿主 exe 所在目录 / plugins；ZIP 布局下 InstallDir 同路径）。
/// 逻辑体显式接收该路径，测试注入临时目录。
pub fn default_plugins_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_default()
        .join("plugins")
}

// ---------------------------------------------------------------------------
// 状态文件（spec §3.2：只读解析、损坏回退空集、原子写）
// ---------------------------------------------------------------------------

/// 状态文件形状（§3.2 `{ "disabled": ["id", ...] }`）。
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
struct ModuleState {
    #[serde(default)]
    disabled: Vec<String>,
}

/// 读取禁用集合：文件缺失 → 空集；损坏/不可读 → 空集 + stderr 告警
/// （不信任状态文件内容，§5.2「状态文件：只读解析 + 损坏回退空集」）。
/// 按需读取，无全局缓存（决议：load on demand，keep it simple）。
pub fn load_module_state(dir: &Path) -> HashSet<String> {
    let path = dir.join(MODULE_STATE_FILE);
    let raw = match fs::read(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return HashSet::new(),
        Err(e) => {
            eprintln!(
                "WARN ab-app: 模块状态文件不可读（回退空集）：{e}：{}",
                path.display()
            );
            return HashSet::new();
        }
    };
    match serde_json::from_slice::<ModuleState>(&raw) {
        Ok(state) => state.disabled.into_iter().collect(),
        Err(e) => {
            eprintln!(
                "WARN ab-app: 模块状态文件损坏（回退空集）：{e}：{}",
                path.display()
            );
            HashSet::new()
        }
    }
}

/// 原子写禁用集合（§3.2）：tmp + rename。`std::fs::rename` 在 Windows 为
/// `MoveFileExW(MOVEFILE_REPLACE_EXISTING)` 覆盖式替换、POSIX 为原子覆盖，
/// 无「先删旧再写新」的崩溃丢文件窗口；失败清理 tmp，不触碰旧文件。
pub fn save_module_state(dir: &Path, disabled: &HashSet<String>) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let mut ids: Vec<String> = disabled.iter().cloned().collect();
    ids.sort();
    let json =
        serde_json::to_vec_pretty(&ModuleState { disabled: ids }).map_err(io::Error::other)?;
    let path = dir.join(MODULE_STATE_FILE);
    let tmp = dir.join(".ab-modules.json.tmp");
    let write = (|| -> io::Result<()> {
        let mut f = File::create(&tmp)?;
        f.write_all(&json)?;
        f.flush()?;
        f.sync_all()?;
        Ok(())
    })();
    if let Err(e) = write {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    if let Err(e) = fs::rename(&tmp, &path) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// 禁用 id 集合的公开读取入口（`list_plugins` 合并展示数据源）：读取
/// `<plugins_dir>/.ab-modules.json`（缺失/损坏回退空集，同
/// [`load_module_state`]）。
pub fn load_disabled_ids(plugins_dir: &Path) -> HashSet<String> {
    load_module_state(plugins_dir)
}

// ---------------------------------------------------------------------------
// ZIP 解压管线（spec §4.2 步骤 ①②③④）
// ---------------------------------------------------------------------------

/// 解压管线失败（§5.1 全部映射 `module_install`；T6 更新流复用）。
#[derive(Debug)]
pub enum ZipError {
    /// 限额超限（大小/条目数）或 ZIP 中心目录非法。
    Limits(String),
    /// zip-slip：条目越出目标目录（绝对路径/`..`，`enclosed_name` 拒绝）。
    ZipSlip(String),
    /// 解压/元数据 IO 失败。
    Io(io::Error),
    /// 根 plugin.json 缺失或宿主校验失败。
    Manifest(ab_host::DiscoveryError),
}

impl std::fmt::Display for ZipError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ZipError::Limits(m) => write!(f, "zip limits: {m}"),
            ZipError::ZipSlip(entry) => {
                write!(f, "zip-slip: entry `{entry}` escapes the target directory")
            }
            ZipError::Io(e) => write!(f, "zip io: {e}"),
            ZipError::Manifest(e) => write!(f, "zip manifest: {e}"),
        }
    }
}

impl std::error::Error for ZipError {}

impl From<io::Error> for ZipError {
    fn from(e: io::Error) -> Self {
        ZipError::Io(e)
    }
}

/// 解压 `zip_path` 到 `dest_dir`（必须为空/不存在），返回根 plugin.json
/// 声明的 plugin id。目录名=id 由调用方保证（以返回 id 命名最终目录）。
///
/// 步骤 ①②③④：限额 → zip-slip → 解压 → 根 plugin.json 解析 + 宿主校验
/// （与发现扫描同函数：load_manifest / validate / resolve_entry）。
pub fn extract_plugin_zip(zip_path: &Path, dest_dir: &Path) -> Result<String, ZipError> {
    // ① 限额：文件大小（解压前按 ZIP 自身大小判定）。
    let meta = fs::metadata(zip_path)?;
    if !meta.is_file() {
        return Err(ZipError::Limits("not a regular file".to_string()));
    }
    if meta.len() > ZIP_MAX_BYTES {
        return Err(ZipError::Limits(format!(
            "zip size {} exceeds limit of {ZIP_MAX_BYTES} bytes",
            meta.len()
        )));
    }
    let file = File::open(zip_path)?;
    let mut archive = ZipArchive::new(file)
        .map_err(|e| ZipError::Limits(format!("not a valid zip archive: {e}")))?;
    // ① 限额：条目数。
    if archive.len() > ZIP_MAX_ENTRIES {
        return Err(ZipError::Limits(format!(
            "zip has {} entries, exceeding limit of {ZIP_MAX_ENTRIES}",
            archive.len()
        )));
    }
    fs::create_dir_all(dest_dir)?;

    // ② zip-slip：`enclosed_name` 拒绝绝对路径与 `..` 越界（None）；
    // 目录条目建目录，其余写文件（条目名经 enclosed 规范化）。
    // 膨胀防护：per-entry 按中心目录声明的 uncompressed size（entry.size()）
    // 在写盘前判定；累计解压尺寸逐条目累积，超限即止（不解压、不落盘）。
    let mut total_unpacked: u64 = 0;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|e| ZipError::Io(io::Error::other(e)))?;
        let Some(name) = entry.enclosed_name() else {
            return Err(ZipError::ZipSlip(entry.name().to_string()));
        };
        let unpacked = entry.size();
        if unpacked > UNPACKED_ENTRY_MAX_BYTES {
            return Err(ZipError::Limits(format!(
                "unpacked entry `{}` size {unpacked} exceeds limit of {UNPACKED_ENTRY_MAX_BYTES} bytes",
                entry.name()
            )));
        }
        total_unpacked += unpacked;
        if total_unpacked > TOTAL_UNPACKED_MAX_BYTES {
            return Err(ZipError::Limits(format!(
                "total unpacked size {total_unpacked} exceeds limit of {TOTAL_UNPACKED_MAX_BYTES} bytes"
            )));
        }
        let out_path = dest_dir.join(name);
        if entry.is_dir() {
            fs::create_dir_all(&out_path)?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut out = File::create(&out_path)?;
        io::copy(&mut entry, &mut out)?;
    }

    // ④ 根 plugin.json：解析 + 宿主校验（与 discovery::scan_plugin 同函数集）。
    let mut manifest = ab_host::manifest::load_manifest(dest_dir).map_err(ZipError::Manifest)?;
    ab_host::manifest::validate(&manifest).map_err(ZipError::Manifest)?;
    ab_host::manifest::normalize_match_rules(&mut manifest.r#match);
    ab_host::manifest::resolve_entry(&manifest, dest_dir).map_err(ZipError::Manifest)?;
    Ok(manifest.id)
}

/// `extract_plugin_zip` 失败 → `module_install`（§5.1）。
fn install_error(e: impl std::fmt::Display) -> IpcError {
    module_error("module_install", format!("plugin zip rejected: {e}"))
}

// ---------------------------------------------------------------------------
// 命令
// ---------------------------------------------------------------------------

/// `install_plugin_zip`（spec §4.1/§4.2）：安装本地 ZIP；`overwrite=true`
/// 覆盖不同版本（同版本恒「已安装」冲突）。返回新模块 `PluginInfoDto`。
#[tauri::command(rename_all = "snake_case")]
pub async fn install_plugin_zip(
    discovery: tauri::State<'_, Arc<PluginRegistry>>,
    coordinator: tauri::State<'_, Arc<ImportCoordinator>>,
    path: String,
    overwrite: bool,
) -> Result<PluginInfoDto, IpcError> {
    install_plugin_zip_logic(
        coordinator.inner(),
        discovery.inner(),
        &default_plugins_dir(),
        &path,
        overwrite,
    )
    .await
}

/// `install_plugin_zip` 逻辑体（handler 薄包装；plugins_dir 显式注入供测试）。
/// `coordinator` 本步未用（T6 更新流复用管线时用于会话重启），保留签名。
#[allow(unused_variables)]
pub async fn install_plugin_zip_logic(
    coordinator: &ImportCoordinator,
    registry: &PluginRegistry,
    plugins_dir: &Path,
    path: &str,
    overwrite: bool,
) -> Result<PluginInfoDto, IpcError> {
    let path = path.trim();
    if path.is_empty() {
        return Err(IpcError::invalid_arg("path must not be empty"));
    }
    if !Path::new(path).is_file() {
        return Err(module_error(
            "module_install",
            format!("zip file not found: {path}"),
        ));
    }

    // ③ 解压到 plugins/ 下临时目录（同卷保证 rename 原子搬入；崩溃残留
    // 的 .tmp 目录下次扫描进 invalid 列表，spec §5.2 崩溃兜底）。
    let tmp = plugins_dir.join(format!(
        ".tmp-install-{}-{}",
        std::process::id(),
        now_nanos()
    ));
    let result = extract_plugin_zip(Path::new(path), &tmp);
    let manifest = match result {
        Ok(id) => {
            // extract 已通过 load_manifest/validate/resolve_entry；此处再取
            // 完整 manifest（版本/update_url 供冲突判定与 DTO）。
            match ab_host::manifest::load_manifest(&tmp) {
                Ok(manifest) => (id, manifest),
                Err(e) => {
                    let _ = fs::remove_dir_all(&tmp);
                    return Err(install_error(e));
                }
            }
        }
        Err(e) => {
            let _ = fs::remove_dir_all(&tmp);
            return Err(install_error(e));
        }
    };
    let (id, manifest) = manifest;
    let dest = plugins_dir.join(&id);

    // ⑤ 冲突判定（spec §3.4）：内建 → 拒绝；同版本 → 已安装；
    // 不同版本 → 无 overwrite 拒绝，有则继续。
    if crate::BUILTIN_PLUGIN_IDS.contains(&id.as_str()) {
        let _ = fs::remove_dir_all(&tmp);
        return Err(module_error(
            "module_protected",
            format!("plugin `{id}` is builtin and cannot be installed or replaced"),
        ));
    }
    if dest.exists() {
        let existing_version = ab_host::manifest::load_manifest(&dest)
            .ok()
            .map(|m| m.version);
        if existing_version.as_deref() == Some(manifest.version.as_str()) {
            let _ = fs::remove_dir_all(&tmp);
            return Err(module_error(
                "module_conflict",
                format!("plugin `{id}` v{} is already installed", manifest.version),
            ));
        }
        if !overwrite {
            let _ = fs::remove_dir_all(&tmp);
            let current = existing_version.unwrap_or_else(|| "unknown".to_string());
            return Err(module_error(
                "module_conflict",
                format!("plugin `{id}` is already installed (v{current}); pass overwrite=true to replace"),
            ));
        }
    }

    // ⑥ 原子搬入：先删旧再 rename（同卷 rename 原子；失败清理临时目录）。
    if dest.exists() {
        if let Err(e) = fs::remove_dir_all(&dest) {
            let _ = fs::remove_dir_all(&tmp);
            return Err(module_error(
                "module_install",
                format!("cannot remove existing plugin dir: {e}"),
            ));
        }
    }
    if let Err(e) = fs::rename(&tmp, &dest) {
        let _ = fs::remove_dir_all(&tmp);
        return Err(module_error(
            "module_install",
            format!("cannot move plugin into place: {e}"),
        ));
    }

    // ⑦ 重扫：新模块进入发现列表并广播 PluginsReloaded（§1.5）。
    registry.reload();

    Ok(PluginInfoDto::from_parts(
        id.clone(),
        manifest.display_name.clone(),
        manifest.version.clone(),
        "discovered".to_string(),
        Vec::new(),
        None,
        "portable",
        false,
        registry.is_disabled(&id),
        manifest.update_url.clone(),
        manifest.author.clone(),
        manifest.repository.clone(),
        manifest.tools.clone(),
        manifest.changelog.clone(),
    ))
}

/// `uninstall_plugin`（spec §4.4）：关闭该插件全部文件会话 → 终止插件进程
/// （shutdown_plugin_sessions，live 进程 CWD 句柄会阻塞删目录）→ 删目录 →
/// reload；内建拒绝（`module_protected`）、目录不存在 → `module_not_found`、
/// 清理失败 → `module_in_use`。
#[tauri::command(rename_all = "snake_case")]
pub async fn uninstall_plugin(
    discovery: tauri::State<'_, Arc<PluginRegistry>>,
    coordinator: tauri::State<'_, Arc<ImportCoordinator>>,
    plugin_id: String,
) -> Result<(), IpcError> {
    uninstall_plugin_logic(
        coordinator.inner(),
        discovery.inner(),
        &default_plugins_dir(),
        &plugin_id,
    )
    .await
}

/// `uninstall_plugin` 逻辑体（handler 薄包装）。
///
/// 会话关闭范围：逐文件 `unload_file`（状态清理）→ `shutdown_plugin_sessions`
/// （进程级终止：host_sessions 表移除 + 会话 shutdown，镜像 reload_session 的
/// 旧实例停机）。不依赖空闲回收（300s）——live 进程持有插件目录 CWD 句柄时
/// Windows 下 `remove_dir_all` 必失败（旧语义 → `module_in_use`，review 修复）。
pub async fn uninstall_plugin_logic(
    coordinator: &ImportCoordinator,
    registry: &PluginRegistry,
    plugins_dir: &Path,
    plugin_id: &str,
) -> Result<(), IpcError> {
    let plugin_id = plugin_id.trim();
    if plugin_id.is_empty() {
        return Err(IpcError::invalid_arg("plugin_id must not be empty"));
    }
    if crate::BUILTIN_PLUGIN_IDS.contains(&plugin_id) {
        return Err(module_error(
            "module_protected",
            format!("plugin `{plugin_id}` is builtin and cannot be uninstalled"),
        ));
    }
    let dest = plugins_dir.join(plugin_id);
    if !dest.exists() {
        return Err(module_error(
            "module_not_found",
            format!("plugin `{plugin_id}` is not installed"),
        ));
    }

    // 关闭该插件全部文件会话（卸载前置清理，§4.4）。
    for file_id in coordinator.file_index().files_of(plugin_id) {
        coordinator.unload_file(&file_id).await;
    }
    // 终止插件进程（live 会话 CWD 句柄占用目录，不终止则删目录失败）。
    coordinator.shutdown_plugin_sessions(plugin_id).await;

    if let Err(e) = fs::remove_dir_all(&dest) {
        return Err(module_error(
            "module_in_use",
            format!("cannot remove plugin dir (session cleanup incomplete?): {e}"),
        ));
    }
    registry.reload();
    Ok(())
}

/// `set_plugin_enabled`（spec §4.4/§3.2）：写状态文件 → `set_disabled`（触发
/// reload + PluginsReloaded）。内建模块仅可禁用不可卸载，故无保护限制。
/// 卸载不清理状态文件（重装后保持禁用意愿，语义幂等）。
#[tauri::command(rename_all = "snake_case")]
pub fn set_plugin_enabled(
    discovery: tauri::State<'_, Arc<PluginRegistry>>,
    plugin_id: String,
    enabled: bool,
) -> Result<(), IpcError> {
    set_plugin_enabled_logic(
        discovery.inner(),
        &default_plugins_dir(),
        &plugin_id,
        enabled,
    )
}

/// `set_plugin_enabled` 逻辑体（handler 薄包装）。
pub fn set_plugin_enabled_logic(
    registry: &PluginRegistry,
    plugins_dir: &Path,
    plugin_id: &str,
    enabled: bool,
) -> Result<(), IpcError> {
    let plugin_id = plugin_id.trim();
    if plugin_id.is_empty() {
        return Err(IpcError::invalid_arg("plugin_id must not be empty"));
    }
    if !plugins_dir.join(plugin_id).exists() {
        return Err(module_error(
            "module_not_found",
            format!("plugin `{plugin_id}` is not installed"),
        ));
    }
    let mut disabled = load_module_state(plugins_dir);
    if enabled {
        disabled.remove(plugin_id);
    } else {
        disabled.insert(plugin_id.to_string());
    }
    save_module_state(plugins_dir, &disabled)
        .map_err(|e| module_error("state_io", format!("cannot write module state file: {e}")))?;
    let ids: Vec<String> = disabled.into_iter().collect();
    registry.set_disabled(&ids);
    Ok(())
}

/// 无 rand 依赖的纳秒时间戳（临时目录命名唯一性用）。
fn now_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// §5.1 模块管理错误码表快照：六个码位形状一致（code/message/data）。
    #[test]
    fn module_error_codes_match_section5_1() {
        for code in [
            "module_install",
            "module_conflict",
            "module_protected",
            "module_in_use",
            "state_io",
            "module_not_found",
        ] {
            let e = module_error(code, "boom");
            assert_eq!(e.code, code);
            assert_eq!(e.message, "boom");
            assert!(e.data.is_none(), "模块管理错误不携带 data");
        }
    }

    #[test]
    fn state_file_shapes_match_section3_2() {
        let dir = std::env::temp_dir().join(format!("ab-state-shape-{}", now_nanos()));
        let set: HashSet<String> = ["a".to_string(), "b".to_string()].into_iter().collect();
        save_module_state(&dir, &set).expect("save");
        let raw = fs::read_to_string(dir.join(MODULE_STATE_FILE)).expect("read");
        let value: serde_json::Value = serde_json::from_str(&raw).expect("json");
        assert_eq!(
            value,
            serde_json::json!({ "disabled": ["a", "b"] }),
            "§3.2 形状：{{ \"disabled\": [...] }}（排序稳定）"
        );
        assert_eq!(load_module_state(&dir), set);
        let _ = fs::remove_dir_all(&dir);
    }
}
