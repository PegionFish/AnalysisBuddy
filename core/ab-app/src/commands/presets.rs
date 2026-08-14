//! 用户预设存储命令（Wave 2 C5）：`list_user_presets` / `save_user_preset` /
//! `delete_user_preset`，文件 `%APPDATA%\AnalysisBuddy\presets\<id>.abpreset.json`
//! （与插件预设同构，见 `ab_protocol::manifest::PresetDef`——id/name/description
//! 形状一致，entries 按 plugin_id 分键）。
//!
//! 存储模式照抄 `plugin_manager.rs` 状态文件：只读解析 + 损坏回落（list 跳过
//! 损坏/不可读文件并 stderr 告警）、tmp + rename 原子写、失败清理 tmp、
//! `serde_json::to_vec_pretty`（UTF-8 无 BOM）。
//! 错误码 `preset_conflict` 为命令层自有码（`module_error` 先例，§1.10 表外），
//! message 带 id。

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use ab_protocol::manifest::LocalizedName;
use tokio::sync::Mutex as AsyncMutex;

use crate::commands::IpcError;
use crate::ipc_errors::module_error;

/// 用户预设文件名后缀（`<id>.abpreset.json`）。
const PRESET_FILE_SUFFIX: &str = ".abpreset.json";
/// id 长度上限（正则 `{0,63}` 的首字符外余长；正则本身即 ≤64 全长）。
const ID_MAX_LEN: usize = 64;

// ---------------------------------------------------------------------------
// 命令层互斥锁（照抄 `plugin_manager.rs` 的 `PLUGIN_LOCKS` 模式）：每预设 id
// 一把 `tokio` 异步互斥，save/delete 整条流程按 id 串行——同进程并发
// save 同名（同 id）时后到者在锁内看到既有文件，稳定得到 `preset_conflict`，
// 杜绝两个写者的 tmp 文件交叉/互相 rename。list 只读不加锁。
// std `Mutex` 只保护 HashMap 查询/插入瞬间，不跨 await 持有。
// ---------------------------------------------------------------------------
static PRESET_LOCKS: OnceLock<Mutex<HashMap<String, Arc<AsyncMutex<()>>>>> = OnceLock::new();

/// 取 `id` 对应的命令层互斥锁（懒建）。
fn preset_lock(id: &str) -> Arc<AsyncMutex<()>> {
    let map = PRESET_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = map.lock().expect("preset lock map poisoned");
    guard
        .entry(id.to_string())
        .or_insert_with(|| Arc::new(AsyncMutex::new(())))
        .clone()
}

/// 用户预设（文件 `%APPDATA%\AnalysisBuddy\presets\<id>.abpreset.json`；
/// 与插件预设同构——id/name/description 形状一致，entries 按 plugin_id 分键）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct UserPresetDto {
    /// `^[a-z0-9][a-z0-9-_]{0,63}$`；文件名 = `<id>.abpreset.json`。
    pub id: String,
    /// 强制双语名。
    pub name: ab_protocol::manifest::LocalizedName,
    /// 可选双语描述。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<ab_protocol::manifest::LocalizedName>,
    /// plugin_id → 该插件 metric 候选列表（用户保存天然精确，无模糊项）。
    #[serde(default)]
    pub entries: std::collections::HashMap<String, Vec<String>>,
}

/// 用户预设目录：照抄 `ab-host::discovery.rs` 的 APPDATA 公式 +
/// `.join("presets")`（`%APPDATA%\AnalysisBuddy\presets`）。
pub fn presets_dir() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join("AnalysisBuddy")
        .join("presets")
}

/// id 合法性（`^[a-z0-9][a-z0-9-_]{0,63}$` 手工实现，无 regex 依赖）：
/// 首字符 [a-z0-9]，余字符 [a-z0-9-_]，全长 ≤64。`/`、`\`、`.`（含 `..`
/// 任意形态、空串、大写）天然不命中。
fn valid_preset_id(id: &str) -> bool {
    let mut chars = id.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }
    let mut len = 1usize;
    for c in chars {
        if !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_') {
            return false;
        }
        len += 1;
        if len > ID_MAX_LEN {
            return false;
        }
    }
    true
}

/// id 由名称生成（slug 化）：小写、非 [a-z0-9-] 转 '-'、连续 '-' 折叠、
/// 去首尾 '-'（pending_dash 只在后随字母/数字时落笔）；空 → "preset"；
/// 截断 64。源取 `name.zh`（保存侧已校验 trim 后非空）。
fn slugify_id(name: &str) -> String {
    let mut out = String::new();
    let mut pending_dash = false;
    for ch in name.chars() {
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_lowercase() || lower.is_ascii_digit() {
            // 首字符前的 '-' 不落笔（去首 '-'）；连续 '-' 经 pending 折叠。
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            pending_dash = false;
            out.push(lower);
        } else {
            pending_dash = true;
        }
    }
    // 末尾残留 pending '-' 不落笔（去尾 '-'）。
    if out.is_empty() {
        out.push_str("preset");
    }
    out.truncate(ID_MAX_LEN);
    out
}

/// `list_user_presets` 逻辑体（dir 注入供测试）：扫描 `<dir>/*.abpreset.json`，
/// 损坏/不可读/文件名 id 非法或与文件内 id 不符 → 跳过 + stderr 告警
/// （回落空集不崩溃）；目录缺失 → 空集。按 id 排序返回。
pub fn list_user_presets_logic(dir: &Path) -> Vec<UserPresetDto> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => {
            eprintln!(
                "WARN ab-app: 用户预设目录不可读（回退空集）：{e}：{}",
                dir.display()
            );
            return Vec::new();
        }
    };
    let mut presets: Vec<UserPresetDto> = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let Some(id) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(PRESET_FILE_SUFFIX).map(String::from))
        else {
            continue;
        };
        if !valid_preset_id(&id) {
            continue;
        }
        let raw = match fs::read(&path) {
            Ok(raw) => raw,
            Err(e) => {
                eprintln!("WARN ab-app: 用户预设不可读（跳过）：{e}：{}", path.display());
                continue;
            }
        };
        match serde_json::from_slice::<UserPresetDto>(&raw) {
            Ok(preset) if preset.id == id => presets.push(preset),
            Ok(_) => eprintln!(
                "WARN ab-app: 用户预设文件内 id 与文件名不符（跳过）：{}",
                path.display()
            ),
            Err(e) => eprintln!("WARN ab-app: 用户预设损坏（跳过）：{e}：{}", path.display()),
        }
    }
    presets.sort_by(|a, b| a.id.cmp(&b.id));
    presets
}

/// save 前置（`save_user_preset_logic` 与命令层锁包装共用，保证锁键与
/// 落盘 id 一致）：`name.zh`/`name.en` trim 后须非空（否则 reject
/// `invalid_arg`）；返回 id = `name.zh` slug 化产物。
fn prepare_save(name: &LocalizedName) -> Result<String, IpcError> {
    let zh = name.zh.trim();
    let en = name.en.trim();
    if zh.is_empty() || en.is_empty() {
        return Err(IpcError::invalid_arg(
            "name.zh and name.en must be non-empty after trimming",
        ));
    }
    Ok(slugify_id(zh))
}

/// `save_user_preset` 逻辑体（dir 注入供测试）：
/// ① `prepare_save`（双语名校验 + id 生成）；
/// ② 同 id 文件已存在 → reject `preset_conflict`（message 带 id）；
/// ③ 目录不存在则创建；tmp + rename 原子写（UTF-8 无 BOM），失败清理 tmp。
pub fn save_user_preset_logic(
    dir: &Path,
    name: LocalizedName,
    entries: HashMap<String, Vec<String>>,
) -> Result<UserPresetDto, IpcError> {
    let id = prepare_save(&name)?;
    let path = dir.join(format!("{id}{PRESET_FILE_SUFFIX}"));
    let tmp = dir.join(format!("{id}{PRESET_FILE_SUFFIX}.tmp"));
    if path.exists() {
        return Err(module_error(
            "preset_conflict",
            format!("user preset `{id}` already exists"),
        ));
    }
    let preset = UserPresetDto {
        id,
        name,
        description: None,
        entries,
    };
    fs::create_dir_all(dir)
        .map_err(|e| module_error("state_io", format!("cannot create user preset dir: {e}")))?;
    let json = serde_json::to_vec_pretty(&preset)
        .map_err(|e| module_error("internal", format!("cannot serialize user preset: {e}")))?;
    let write = (|| -> io::Result<()> {
        let mut f = File::create(&tmp)?;
        f.write_all(&json)?;
        f.flush()?;
        f.sync_all()?;
        Ok(())
    })();
    if let Err(e) = write {
        let _ = fs::remove_file(&tmp);
        return Err(module_error(
            "state_io",
            format!("cannot write user preset file: {e}"),
        ));
    }
    if let Err(e) = fs::rename(&tmp, &path) {
        let _ = fs::remove_file(&tmp);
        return Err(module_error(
            "state_io",
            format!("cannot move user preset into place: {e}"),
        ));
    }
    Ok(preset)
}

/// `delete_user_preset` 逻辑体（dir 注入供测试）：id 不匹配
/// `^[a-z0-9][a-z0-9-_]{0,63}$`（含 `/`、`\`、`..` 任意形态、空串、大写）→
/// reject `invalid_arg`；文件不存在 → 幂等 `Ok`；删除失败 → `state_io`。
pub fn delete_user_preset_logic(dir: &Path, id: &str) -> Result<(), IpcError> {
    if !valid_preset_id(id) {
        return Err(IpcError::invalid_arg(format!(
            "invalid user preset id `{id}` (must match ^[a-z0-9][a-z0-9-_]{{0,63}}$)"
        )));
    }
    let path = dir.join(format!("{id}{PRESET_FILE_SUFFIX}"));
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(module_error(
            "state_io",
            format!("cannot delete user preset `{id}`: {e}"),
        )),
    }
}

/// `save_user_preset` 命令体（dir 注入 + 命令层互斥）：id 在
/// `prepare_save` **之后**才确定，故在生成 id 之后再取锁——锁覆盖
/// 「exists 检查 + 原子写」全程；并发同名 save（同 id）串行后后到者
/// 稳定得到 `preset_conflict`，杜绝 tmp 文件交叉。
pub async fn save_user_preset_locked(
    dir: &Path,
    name: LocalizedName,
    entries: HashMap<String, Vec<String>>,
) -> Result<UserPresetDto, IpcError> {
    let id = prepare_save(&name)?;
    let lock = preset_lock(&id);
    let _guard = lock.lock().await;
    save_user_preset_logic(dir, name, entries)
}

/// `delete_user_preset` 命令体（dir 注入 + 命令层互斥）：按 id 取锁，
/// 锁覆盖「校验存在 + 删除」全程（并发 delete/save 同 id 串行）。
pub async fn delete_user_preset_locked(dir: &Path, id: &str) -> Result<(), IpcError> {
    let lock = preset_lock(id);
    let _guard = lock.lock().await;
    delete_user_preset_logic(dir, id)
}

/// 列出 `%APPDATA%\AnalysisBuddy\presets\*.abpreset.json`；损坏/不可读文件
/// 跳过 + stderr 诊断（回落空集）；按 id 排序返回。
#[tauri::command(rename_all = "snake_case")]
pub async fn list_user_presets() -> Result<Vec<UserPresetDto>, IpcError> {
    Ok(list_user_presets_logic(&presets_dir()))
}

/// 保存用户预设：id 由 name 生成（slug 化）；重名（同 id 已有文件）→
/// reject `preset_conflict`；目录不存在则创建；tmp + rename 原子写。
#[tauri::command(rename_all = "snake_case")]
pub async fn save_user_preset(
    name: LocalizedName,
    entries: HashMap<String, Vec<String>>,
) -> Result<UserPresetDto, IpcError> {
    save_user_preset_locked(&presets_dir(), name, entries).await
}

/// 删除 `<id>.abpreset.json`；文件不存在 → 幂等 `Ok`；id 非法（不匹配
/// `^[a-z0-9][a-z0-9-_]{0,63}$`，含 `/`、`\`、`..` 任意形态）→ reject
/// `invalid_arg`。
#[tauri::command(rename_all = "snake_case")]
pub async fn delete_user_preset(id: String) -> Result<(), IpcError> {
    delete_user_preset_locked(&presets_dir(), &id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_rules() {
        assert_eq!(slugify_id("My Preset!"), "my-preset");
        assert_eq!(slugify_id("测试"), "preset", "全非 ASCII → 折叠后为空 → preset");
        assert_eq!(slugify_id("A  B"), "a-b", "连续空白折叠为单 '-'");
        assert_eq!(slugify_id("   "), "preset", "纯空白 → 空 → preset");
        assert_eq!(slugify_id("-lead-t"), "lead-t", "去首 '-'");
        assert_eq!(slugify_id("trail-"), "trail", "去尾 '-'");
        let long = format!("{}-{}", "a".repeat(40), "b".repeat(40));
        let slug = slugify_id(&long);
        assert_eq!(slug.len(), 64, "截断 64");
        assert!(valid_preset_id(&slug), "截断产物仍是合法 id");
    }

    #[test]
    fn preset_id_validation() {
        for good in ["a", "a-b_c", "0abc", "a".repeat(64).as_str()] {
            assert!(valid_preset_id(good), "{good:?} 应合法");
        }
        for bad in [
            "",
            "A",
            "-abc",
            "_abc",
            "a/b",
            "a\\b",
            "a.b",
            "..",
            "a ".repeat(22).trim_end(),
            "a".repeat(65).as_str(),
        ] {
            assert!(!valid_preset_id(bad), "{bad:?} 应非法");
        }
    }

    fn tmp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ab-app-presets-lock-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn name(zh: &str, en: &str) -> LocalizedName {
        LocalizedName {
            zh: zh.to_string(),
            en: en.to_string(),
        }
    }

    /// 并发 save 同名（同 id）：命令层互斥串行后恰好一个 Ok、另一个
    /// Err(preset_conflict)（或 state_io），且最终磁盘文件是完整合法 JSON
    /// （不被交叉残片破坏）。循环多跑断言稳定性。
    #[tokio::test]
    async fn concurrent_save_same_name_serializes_and_writes_complete_file() {
        for round in 0..5 {
            let dir = tmp_dir(&format!("round{round}"));
            let (a, b) = tokio::join!(
                save_user_preset_locked(&dir, name("My Preset!", "My Preset!"), HashMap::new()),
                save_user_preset_locked(&dir, name("My Preset!", "My Preset!"), HashMap::new()),
            );
            let oks = [&a, &b].iter().filter(|r| r.is_ok()).count();
            let errs = [&a, &b].iter().filter(|r| r.is_err()).count();
            assert_eq!(oks, 1, "round {round}: 恰好一个 Ok");
            assert_eq!(errs, 1, "round {round}: 恰好一个 Err");
            for r in [&a, &b] {
                if let Err(e) = r {
                    assert!(
                        e.code == "preset_conflict" || e.code == "state_io",
                        "round {round}: 后到者必须 preset_conflict 或 state_io：{e:?}"
                    );
                }
            }
            let path = dir.join("my-preset.abpreset.json");
            let raw = fs::read(&path).expect("round {round}: 文件必须落盘");
            let parsed: serde_json::Value =
                serde_json::from_slice(&raw).expect("round {round}: 完整合法 JSON");
            assert_eq!(parsed["id"], serde_json::json!("my-preset"));
            fs::remove_dir_all(&dir).expect("cleanup");
        }
    }

    /// delete 与 save 同 id 并发：互斥串行后结果自洽（先删后存 → 文件存在；
    /// 先存后删 → 文件消失）。
    #[tokio::test]
    async fn concurrent_delete_and_save_same_id_serialize() {
        let dir = tmp_dir("del-save");
        let (_, del) = tokio::join!(
            save_user_preset_locked(&dir, name("My Preset!", "My Preset!"), HashMap::new()),
            delete_user_preset_locked(&dir, "my-preset"),
        );
        // 两个顺序都可能（取决于谁先拿锁），但都不能失败得可疑：
        // delete 幂等 Ok；save 要么 Ok 要么 preset_conflict。
        if let Err(e) = del {
            panic!("delete 必须 Ok（幂等）：{e:?}");
        }
        // 无论顺序，目录里最多一个文件且必为完整合法 JSON。
        let mut files: Vec<PathBuf> = fs::read_dir(&dir)
            .expect("read dir")
            .filter_map(Result::ok)
            .map(|e| e.path())
            .collect();
        files.retain(|p| p.extension().map(|e| e == "json").unwrap_or(false));
        assert!(files.len() <= 1, "不得残留半成品文件：{files:?}");
        if let Some(p) = files.first() {
            let raw = fs::read(p).expect("read json");
            serde_json::from_slice::<serde_json::Value>(&raw)
                .expect("残留文件必须为完整合法 JSON");
        }
        fs::remove_dir_all(&dir).expect("cleanup");
    }
}
