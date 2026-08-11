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

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use ab_host::PluginRegistry;
use tokio::sync::Mutex as AsyncMutex;
use zip::ZipArchive;

use crate::commands::{IpcError, PluginInfoDto};
use crate::ipc_errors::module_error;
use crate::network::{parse_repo_url, tag_to_version, UpdateError, UpdateFetcher};
use crate::pipeline_bridge::{ImportCoordinator, ImportStatus};

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

// ---------------------------------------------------------------------------
// 命令层互斥锁（spec §5.2：同一插件操作以命令层互斥锁串行）
// ---------------------------------------------------------------------------
//
// 每插件一把 `tokio` 异步互斥，经进程级 `OnceLock` 存放（进程内所有
// registry/测试共享同一把锁域——锁只串行同 id 操作，跨实例天然安全）。
// std `Mutex` 仅保护 HashMap 的查询/插入瞬间，不跨 await 持有；
// `tokio` 异步锁跨 await 持有（安装/卸载/启用/更新整条流程串行）。
// 单次调用只取一把锁、无嵌套（死锁不可能）；`check_plugin_update` 只读，
// 不加锁。
static PLUGIN_LOCKS: OnceLock<Mutex<HashMap<String, Arc<AsyncMutex<()>>>>> = OnceLock::new();

/// 取 `plugin_id` 对应的命令层互斥锁（懒建）。
fn plugin_lock(plugin_id: &str) -> Arc<AsyncMutex<()>> {
    let map = PLUGIN_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = map.lock().expect("plugin lock map poisoned");
    guard
        .entry(plugin_id.to_string())
        .or_insert_with(|| Arc::new(AsyncMutex::new(())))
        .clone()
}

/// 更新检查结果（spec §4.3 / docs 09：`check_plugin_update` 返回值）。
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct UpdateInfoDto {
    pub plugin_id: String,
    /// 已装版本（manifest.version 原文）。
    pub current_version: String,
    /// 最新发行版 tag（原始形态，如 `v1.2.0`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_version: Option<String>,
    /// latest > current（semver 比较）。
    pub is_newer: bool,
    /// 选中的 zip 资产文件名。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asset_name: Option<String>,
}

/// 模块管理命令组状态（lib.rs setup 注入生产 [`GitHubFetcher`]，测试注入
/// [`MockFetcher`]）：更新流（T6）唯一网络入口。
pub struct PluginManagerState {
    pub fetcher: Arc<dyn UpdateFetcher>,
}

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

/// `module_conflict`（§5.1）携带分类 data（终审修复 Fix 1）：UI 依赖
/// `data.kind` 区分「同版本已装」（信息提示，无覆盖按钮）与「不同版本
/// 需覆盖」（覆盖确认条）；`data.version` 为既有版本（展示用）。
fn conflict_error(id: &str, version: &str, kind: &'static str, message: String) -> IpcError {
    IpcError {
        code: "module_conflict".to_string(),
        message,
        data: Some(serde_json::json!({
            "plugin_id": id,
            "version": version,
            "kind": kind,
        })),
    }
}

/// 更新不可用（spec §4.3：无 update_url / 资产约束不满足 / 非 semver /
/// 不新于当前 / 禁用）。区别于 `module_not_found`：插件存在但当前无可用更新。
fn update_unavailable(message: impl Into<String>) -> IpcError {
    module_error("update_not_available", message)
}

/// 更新链路网络层失败（fetch / download 的 `UpdateError` 非 NoZipAsset 分支）。
fn network_error(message: impl Into<String>) -> IpcError {
    module_error("network", message)
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
    // 命令层互斥锁（spec §5.2）：id 在 ZIP 解压后才可知，故在此处取锁——
    // 冲突判定/覆盖搬入/重扫整段按 id 串行（并发安装同一插件时后到者
    // 看到既有安装，稳定得到 module_conflict 而非互相覆盖）。
    let lock = plugin_lock(&id);
    let _guard = lock.lock().await;
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
            return Err(conflict_error(
                &id,
                &manifest.version,
                "same_version",
                format!("plugin `{id}` v{} is already installed", manifest.version),
            ));
        }
        if !overwrite {
            let _ = fs::remove_dir_all(&tmp);
            let current = existing_version.unwrap_or_else(|| "unknown".to_string());
            return Err(conflict_error(
                &id,
                &current,
                "different_version",
                format!(
                    "plugin `{id}` is already installed (v{current}); pass overwrite=true to replace"
                ),
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
    // 命令层互斥锁（spec §5.2）：与安装/启用/更新同 id 串行。
    let lock = plugin_lock(plugin_id);
    let _guard = lock.lock().await;
    // 真实目录解析（§4.4，终审修复）：发现列表给出插件实际所在目录
    // （便携源或 UserData 等非便携源）；禁用模块不在发现列表 → 回落
    // 便携路径（<plugins_dir>/<id>）。目录不可得 → module_not_found。
    let dest = registry
        .get(plugin_id)
        .map(|p| p.plugin_dir)
        .unwrap_or_else(|| plugins_dir.join(plugin_id));
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
pub async fn set_plugin_enabled(
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
    .await
}

/// `set_plugin_enabled` 逻辑体（handler 薄包装）。
///
/// 可操作 id 判定（终审修复 Fix 3）：发现列表命中（含 UserData 等非便携源
/// 插件）或便携目录存在（禁用中的便携插件不在发现列表）或状态文件已含该
/// id（禁用中的非便携插件，供「启用」入口）→ 可操作；否则
/// `module_not_found`。
pub async fn set_plugin_enabled_logic(
    registry: &PluginRegistry,
    plugins_dir: &Path,
    plugin_id: &str,
    enabled: bool,
) -> Result<(), IpcError> {
    let plugin_id = plugin_id.trim();
    if plugin_id.is_empty() {
        return Err(IpcError::invalid_arg("plugin_id must not be empty"));
    }
    let known = registry.get(plugin_id).is_some()
        || plugins_dir.join(plugin_id).exists()
        || load_disabled_ids(plugins_dir).contains(plugin_id);
    if !known {
        return Err(module_error(
            "module_not_found",
            format!("plugin `{plugin_id}` is not installed"),
        ));
    }
    // 命令层互斥锁（spec §5.2）：与安装/卸载/更新同 id 串行
    // （状态文件写入在锁内，避免与同 id 卸载/更新竞争）。
    let lock = plugin_lock(plugin_id);
    let _guard = lock.lock().await;
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

/// 更新预检（check/update 共用）：trim → 内建（仅 update）→ 发现命中 →
/// 禁用/未知 → update_url 可解析（GitHub `owner/repo`）。
///
/// 返回 `(owner, repo, manifest)`；禁用插件从发现列表消失，但仍在状态文件
/// 禁用集合 → 以 `update_not_available`（消息点名 disabled）拒绝，不虚构
/// 更新。内建模块发现层可见但不可更新（`module_protected`，仅 update 流）。
fn update_preflight(
    registry: &PluginRegistry,
    plugins_dir: &Path,
    plugin_id: &str,
    builtin_protected: bool,
) -> Result<(String, String, String, ab_protocol::manifest::Manifest), IpcError> {
    let plugin_id = plugin_id.trim();
    if plugin_id.is_empty() {
        return Err(IpcError::invalid_arg("plugin_id must not be empty"));
    }
    if builtin_protected && crate::BUILTIN_PLUGIN_IDS.contains(&plugin_id) {
        return Err(module_error(
            "module_protected",
            format!("plugin `{plugin_id}` is builtin and cannot be updated"),
        ));
    }
    let Some(plugin) = registry.get(plugin_id) else {
        if load_disabled_ids(plugins_dir).contains(plugin_id) {
            return Err(update_unavailable(format!(
                "plugin `{plugin_id}` is disabled and cannot be updated"
            )));
        }
        return Err(module_error(
            "module_not_found",
            format!("plugin `{plugin_id}` is not installed"),
        ));
    };
    let manifest = plugin.manifest;
    let Some(update_url) = manifest.update_url.as_deref() else {
        return Err(update_unavailable(format!(
            "plugin `{plugin_id}` declares no update_url"
        )));
    };
    let Some((owner, repo)) = parse_repo_url(update_url) else {
        return Err(update_unavailable(format!(
            "plugin `{plugin_id}` update_url is not a GitHub repository: {update_url}"
        )));
    };
    Ok((plugin_id.to_string(), owner, repo, manifest))
}

/// 拉取最新发行版并统一错误映射（NoZipAsset → `update_not_available`；
/// 其余网络层失败 → `network`）。
async fn fetch_release(
    fetcher: &dyn UpdateFetcher,
    owner: &str,
    repo: &str,
) -> Result<crate::network::ReleaseInfo, IpcError> {
    match fetcher.fetch_latest_release(owner, repo).await {
        Ok(release) => Ok(release),
        Err(UpdateError::NoZipAsset(n)) => Err(update_unavailable(format!(
            "expected exactly one .zip asset in latest release, found {n}"
        ))),
        Err(e) => Err(network_error(format!("cannot check latest release: {e}"))),
    }
}

/// `check_plugin_update`（spec §4.3 / docs 09）：解析 `update_url` →
/// GitHub Releases 最新发行版 → 恰好一个 zip 资产 → semver 比较，返回
/// `UpdateInfoDto`（latest_version / asset_name / is_newer）。
#[tauri::command(rename_all = "snake_case")]
pub async fn check_plugin_update(
    state: tauri::State<'_, PluginManagerState>,
    discovery: tauri::State<'_, Arc<PluginRegistry>>,
    plugin_id: String,
) -> Result<UpdateInfoDto, IpcError> {
    check_plugin_update_logic(
        state.inner().fetcher.as_ref(),
        discovery.inner(),
        &default_plugins_dir(),
        &plugin_id,
    )
    .await
}

/// `check_plugin_update` 逻辑体（handler 薄包装；fetcher/plugins_dir 注入供测试）。
pub async fn check_plugin_update_logic(
    fetcher: &dyn UpdateFetcher,
    registry: &PluginRegistry,
    plugins_dir: &Path,
    plugin_id: &str,
) -> Result<UpdateInfoDto, IpcError> {
    let (plugin_id, owner, repo, manifest) =
        update_preflight(registry, plugins_dir, plugin_id, false)?;
    let release = fetch_release(fetcher, &owner, &repo).await?;
    let Some(latest) = tag_to_version(&release.tag_name) else {
        return Err(update_unavailable(format!(
            "release tag `{}` is not a semver version",
            release.tag_name
        )));
    };
    let is_newer = tag_to_version(&manifest.version)
        .map(|current| latest > current)
        .unwrap_or(false);
    Ok(UpdateInfoDto {
        plugin_id: plugin_id.to_string(),
        current_version: manifest.version.clone(),
        latest_version: Some(release.tag_name.clone()),
        is_newer,
        asset_name: Some(release.asset_name),
    })
}

/// `update_plugin`（spec §4.3 / docs 09）：下载最新发行版 zip → 走安装管线
/// → 关键校验（ZIP 内 id == 被更新模块 id、版本 > 当前）→ 关闭运行中会话 →
/// 覆盖 → 重扫 → 自动重开该模块驻留文件（会话自动重启），返回新
/// `PluginInfoDto`。
#[tauri::command(rename_all = "snake_case")]
pub async fn update_plugin(
    state: tauri::State<'_, PluginManagerState>,
    discovery: tauri::State<'_, Arc<PluginRegistry>>,
    coordinator: tauri::State<'_, Arc<ImportCoordinator>>,
    plugin_id: String,
) -> Result<PluginInfoDto, IpcError> {
    update_plugin_logic(
        state.inner().fetcher.as_ref(),
        coordinator.inner(),
        discovery.inner(),
        &default_plugins_dir(),
        &plugin_id,
    )
    .await
}

/// `update_plugin` 逻辑体（handler 薄包装）。
pub async fn update_plugin_logic(
    fetcher: &dyn UpdateFetcher,
    coordinator: &ImportCoordinator,
    registry: &PluginRegistry,
    plugins_dir: &Path,
    plugin_id: &str,
) -> Result<PluginInfoDto, IpcError> {
    let (plugin_id, owner, repo, manifest) =
        update_preflight(registry, plugins_dir, plugin_id, true)?;
    // 命令层互斥锁（spec §5.2）：更新整条流程（网络拉取→下载→覆盖搬入→
    // 会话重启）与同 id 安装/卸载/启用串行。
    let lock = plugin_lock(&plugin_id);
    let _guard = lock.lock().await;
    let release = fetch_release(fetcher, &owner, &repo).await?;
    let Some(latest) = tag_to_version(&release.tag_name) else {
        return Err(update_unavailable(format!(
            "release tag `{}` is not a semver version",
            release.tag_name
        )));
    };
    let Some(current) = tag_to_version(&manifest.version) else {
        return Err(update_unavailable(format!(
            "installed version `{}` is not a semver version",
            manifest.version
        )));
    };
    if latest <= current {
        return Err(update_unavailable(format!(
            "plugin `{plugin_id}` is already up to date (v{})",
            manifest.version
        )));
    }

    // 下载到 plugins/ 下临时 ZIP（与插件目录同卷；失败清理后拒绝）。
    let tmp_zip = plugins_dir.join(format!(
        ".tmp-update-{}-{}.zip",
        std::process::id(),
        now_nanos()
    ));
    if let Err(e) = fetcher.download(&release.asset_url, &tmp_zip).await {
        let _ = fs::remove_file(&tmp_zip);
        return Err(network_error(format!(
            "cannot download update asset `{}`: {e}",
            release.asset_name
        )));
    }

    // 解压到 plugins/ 下临时目录（复用安装管线 ①②③④：限额/zip-slip/
    // 解压/根 plugin.json 宿主校验）。
    let tmp_dir = plugins_dir.join(format!(
        ".tmp-update-{}-{}",
        std::process::id(),
        now_nanos()
    ));
    let extracted = extract_plugin_zip(&tmp_zip, &tmp_dir);
    let _ = fs::remove_file(&tmp_zip);
    let zip_id = match extracted {
        Ok(id) => id,
        Err(e) => {
            let _ = fs::remove_dir_all(&tmp_dir);
            return Err(install_error(e));
        }
    };
    // 关键校验 ①：ZIP 内 plugin.json id 必须 == 被更新模块 id。
    if zip_id != plugin_id {
        let _ = fs::remove_dir_all(&tmp_dir);
        return Err(module_error(
            "module_install",
            format!("update zip plugin id `{zip_id}` does not match target `{plugin_id}`"),
        ));
    }
    let zip_manifest = match ab_host::manifest::load_manifest(&tmp_dir) {
        Ok(manifest) => manifest,
        Err(e) => {
            let _ = fs::remove_dir_all(&tmp_dir);
            return Err(install_error(e));
        }
    };
    // 关键校验 ②：ZIP 版本必须严格新于当前（release tag 与 ZIP 内版本双源
    // 校验，tag 已过 → 此处仍可能翻车，如发布事故）。
    let Some(zip_version) = tag_to_version(&zip_manifest.version) else {
        let _ = fs::remove_dir_all(&tmp_dir);
        return Err(update_unavailable(format!(
            "update zip version `{}` is not a semver version",
            zip_manifest.version
        )));
    };
    if zip_version <= current {
        let _ = fs::remove_dir_all(&tmp_dir);
        return Err(update_unavailable(format!(
            "update zip for `{plugin_id}` (v{}) is not newer than installed v{}",
            zip_manifest.version, manifest.version
        )));
    }

    // 会话前置（spec §4.4 卸载语义复用）：先记录驻留文件（file_id → 源
    // 路径），关闭全部文件会话 + 终止插件进程（live 进程 CWD 句柄阻塞删目录）。
    let loaded: Vec<(String, String)> = coordinator
        .file_index()
        .files_of(&plugin_id)
        .into_iter()
        .filter_map(|file_id| coordinator.path_of(&file_id).map(|path| (file_id, path)))
        .collect();
    for (file_id, _) in &loaded {
        coordinator.unload_file(file_id).await;
    }
    coordinator.shutdown_plugin_sessions(&plugin_id).await;

    // 覆盖搬入：先删旧再 rename（同卷原子；失败清理临时目录并还原会话无
    // 需——失败时旧目录可能已删，错误向上抛由用户决定，与 install 一致）。
    let dest = plugins_dir.join(&plugin_id);
    if dest.exists() {
        if let Err(e) = fs::remove_dir_all(&dest) {
            let _ = fs::remove_dir_all(&tmp_dir);
            return Err(module_error(
                "module_install",
                format!("cannot remove existing plugin dir: {e}"),
            ));
        }
    }
    if let Err(e) = fs::rename(&tmp_dir, &dest) {
        let _ = fs::remove_dir_all(&tmp_dir);
        return Err(module_error(
            "module_install",
            format!("cannot move updated plugin into place: {e}"),
        ));
    }

    // 重扫：新版本进入发现列表并广播 PluginsReloaded（§1.5）。
    registry.reload();

    // 会话自动重启（reload_plugin §4.6 语义）：重建实例（停旧→拉起新→注册
    // 新适配器）。从未拉起过宿主会话的插件（如测试注入的 MockSession）spawn
    // 失败属预期——沿用既有会话注册，重开文件仍可复用。
    if let Err(e) = coordinator.reload_session(&plugin_id).await {
        eprintln!("WARN ab-app: 更新后重建会话失败（沿用既有会话注册）：{e}");
    }
    // 重开该模块全部驻留文件（重走 load → parse → freeze；单文件失败不阻塞
    // 其余，WARN 告警）。
    for (file_id, path) in &loaded {
        let outcome = coordinator
            .reopen_file(PathBuf::from(path), &plugin_id)
            .await;
        if outcome.status != ImportStatus::Ready {
            eprintln!(
                "WARN ab-app: 更新后重开文件 {file_id} 未达 Ready（{:?}）：{path}",
                outcome.status
            );
        }
    }

    let fresh = registry.get(&plugin_id).ok_or_else(|| {
        module_error(
            "module_install",
            format!("plugin `{plugin_id}` updated but not discovered after reload"),
        )
    })?;
    Ok(PluginInfoDto::from_parts(
        fresh.manifest.id.clone(),
        fresh.manifest.display_name.clone(),
        fresh.manifest.version.clone(),
        "discovered".to_string(),
        coordinator.file_index().files_of(&plugin_id),
        None,
        crate::commands::plugin_source_name(fresh.source),
        false,
        registry.is_disabled(&plugin_id),
        fresh.manifest.update_url.clone(),
        fresh.manifest.author.clone(),
        fresh.manifest.repository.clone(),
        fresh.manifest.tools.clone(),
        fresh.manifest.changelog.clone(),
    ))
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

    /// §5.1 模块管理错误码表快照：module_conflict 例外（携带冲突分类 data，
    /// 见 `module_conflict_payloads_distinguish_conflict_kinds`），其余码位
    /// 形状一致（code/message/data=None）。
    #[test]
    fn module_error_codes_match_section5_1() {
        for code in [
            "module_install",
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
        let conflict = module_error("module_conflict", "boom");
        assert_eq!(conflict.code, "module_conflict");
        assert!(
            conflict.data.is_none(),
            "module_error 本身不带 data——冲突 data 由安装逻辑经 conflict_error 构造"
        );
    }

    /// 终审修复（Fix 1）：module_conflict 必须携带分类 data，UI 才能区分
    /// 「同版本已装」（只提示）与「不同版本需覆盖」（覆盖确认条）。
    #[test]
    fn module_conflict_payloads_distinguish_conflict_kinds() {
        let same = conflict_error(
            "demo-tool",
            "1.0.0",
            "same_version",
            "same version".to_string(),
        );
        assert_eq!(same.code, "module_conflict");
        assert_eq!(
            same.data.as_ref(),
            Some(&serde_json::json!({
                "plugin_id": "demo-tool",
                "version": "1.0.0",
                "kind": "same_version",
            })),
            "同版本冲突 data 形状（plugin_id/version/kind）"
        );
        let different = conflict_error(
            "demo-tool",
            "1.0.0",
            "different_version",
            "different version".to_string(),
        );
        assert_eq!(
            different.data.as_ref(),
            Some(&serde_json::json!({
                "plugin_id": "demo-tool",
                "version": "1.0.0",
                "kind": "different_version",
            })),
            "不同版本冲突 data 形状（plugin_id/version/kind）"
        );
        assert_ne!(
            same.data, different.data,
            "两分类 data 必须不同：UI 靠 kind 字段区分分支"
        );
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
