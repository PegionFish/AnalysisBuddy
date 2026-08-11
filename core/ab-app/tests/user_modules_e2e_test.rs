//! 用户模块端到端验证（opt-in，本机真实模块）：
//!
//! 从 `AB_E2E_USER_MODULES`（`;` 分隔的模块目录列表）读取真实用户开发的
//! 解析模块（如 AnalysisBuddy_BatteryInfoView / AnalysisBuddy_HWiNFO），
//! 以规范布局（目录名 = manifest id）拷贝进临时 plugins 目录，依次验证：
//!   1) 发现层可发现且非 invalid；
//!   2) 真实导入 → parse → Ready → 指标/序列/key_values 全链路；
//!   3) 模块管理：ZIP 安装（含同版本冲突语义）→ 禁用 → 启用 → 卸载。
//!
//! 运行：`$env:AB_E2E_USER_MODULES="dir1;dir2"; cargo test -p ab-app --test user_modules_e2e -- --ignored`
//! 常规定时任务跳过（#[ignore]），避免 CI 依赖本机模块路径。

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use ab_host::{PluginRegistry, PluginRuntime};
use ab_pipeline::{SessionRegistry, Store};
use ab_protocol::manifest::Manifest;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use ab_app::commands::plugin::list_plugins_logic;
use ab_app::commands::plugin_manager::{
    install_plugin_zip_logic, set_plugin_enabled_logic, uninstall_plugin_logic,
};
use ab_app::commands::query::{key_values_at_logic, query_series_logic};
use ab_app::events::PluginMeta;
use ab_app::pipeline_bridge::{ImportCoordinator, ImportStatus};

static NEXT_TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn unique_dir(tag: &str) -> PathBuf {
    let seq = NEXT_TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "ab-user-e2e-{tag}-{}-{seq}-{nanos}",
        std::process::id()
    ))
}

/// AB_E2E_USER_MODULES 未设置时整体跳过。
fn module_sources() -> Vec<PathBuf> {
    match std::env::var("AB_E2E_USER_MODULES") {
        Ok(v) => v
            .split(';')
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// 读模块 plugin.json，取其 id。
fn manifest_id(dir: &Path) -> String {
    let raw = fs::read_to_string(dir.join("plugin.json"))
        .unwrap_or_else(|e| panic!("模块缺 plugin.json {}: {e}", dir.display()));
    let m: Manifest = serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("模块 plugin.json 解析失败 {}: {e}", dir.display()));
    m.id
}

/// 递归拷贝目录。
fn copy_tree(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).expect("mkdir dst");
    for entry in fs::read_dir(src).expect("read src") {
        let entry = entry.expect("entry");
        let name = entry.file_name();
        if name == ".git" || name == ".pytest_cache" || name == "__pycache__" {
            continue;
        }
        let target = dst.join(&name);
        if entry.file_type().expect("ft").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).expect("copy file");
        }
    }
}

/// 把每个用户模块按规范布局（目录名 = id）拷进 plugins 目录，
/// 并把其 tests/fixtures 拷为 <id>/fixtures 供导入使用。
fn stage_modules(plugins_dir: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    for src in module_sources() {
        let id = manifest_id(&src);
        let dst = plugins_dir.join(&id);
        copy_tree(&src, &dst);
        let fixtures = src.join("tests").join("fixtures");
        if fixtures.is_dir() {
            copy_tree(&fixtures, &dst.join("fixtures"));
        }
        out.push((id, dst));
    }
    out
}

/// 测试环境：真实三源 registry + 真实 coordinator（真实 PluginRuntime，
/// 模块经 python + SDK 真实 spawn）。
fn env(plugins_dir: &Path) -> (Arc<PluginRegistry>, ImportCoordinator) {
    let registry = Arc::new(PluginRegistry::with_sources(
        plugins_dir.to_path_buf(),
        plugins_dir.to_path_buf(),
        plugins_dir.to_path_buf(),
    ));
    let coordinator = ImportCoordinator::new(
        Arc::new(Store::new()),
        Arc::new(SessionRegistry::new()),
        tokio::sync::mpsc::unbounded_channel().0,
        Arc::new(PluginRuntime::new(registry.clone())),
        registry.clone(),
    );
    (registry, coordinator)
}

/// 选导入用 fixture：优先名字含 sample 的文件，否则取 fixtures 下第一个
/// 匹配 manifest extensions 的文件。
fn pick_fixture(module_dir: &Path, m: &Manifest) -> Option<PathBuf> {
    let fixtures = module_dir.join("fixtures");
    if !fixtures.is_dir() {
        return None;
    }
    let mut candidates: Vec<PathBuf> = fs::read_dir(&fixtures)
        .expect("read fixtures")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension()
                .map(|ext| {
                    m.r#match
                        .extensions
                        .iter()
                        .any(|x| x == &ext.to_string_lossy())
                })
                .unwrap_or(false)
        })
        .collect();
    candidates.sort();
    if let Some(pos) = candidates.iter().position(|p| {
        p.file_name()
            .map(|n| n.to_string_lossy().contains("sample"))
            .unwrap_or(false)
    }) {
        return Some(candidates.remove(pos));
    }
    candidates.first().cloned()
}

/// 测试 1：发现层找到用户模块且非 invalid。
#[tokio::test]
#[ignore]
async fn discovery_finds_user_modules() {
    let sources = module_sources();
    if sources.is_empty() {
        eprintln!("SKIP: AB_E2E_USER_MODULES 未设置");
        return;
    }
    let plugins_dir = unique_dir("discover");
    fs::create_dir_all(&plugins_dir).unwrap();
    let staged = stage_modules(&plugins_dir);
    let (registry, _coordinator) = env(&plugins_dir);
    let outcome = registry.discover();
    for (id, _dir) in &staged {
        let found = outcome
            .plugins
            .iter()
            .find(|p| &p.manifest.id == id)
            .unwrap_or_else(|| panic!("模块 {id} 应被发现"));
        assert!(
            outcome.invalid.is_empty(),
            "模块不应进 invalid 列表: {:?}",
            outcome.invalid
        );
        eprintln!(
            "[ok] 发现 {id} v{} @ {}",
            found.manifest.version,
            found.plugin_dir.display()
        );
    }
    eprintln!(
        "[ok] 发现模块数: {}（invalid: {:?}, shadowed: {:?}）",
        outcome.plugins.len(),
        outcome.invalid.len(),
        outcome.shadowed.len()
    );
}

/// 测试 2：真实导入 → parse → Ready → 指标/序列/key_values 全链路。
#[tokio::test]
#[ignore]
async fn user_modules_import_parse_query_e2e() {
    let sources = module_sources();
    if sources.is_empty() {
        eprintln!("SKIP: AB_E2E_USER_MODULES 未设置");
        return;
    }
    let plugins_dir = unique_dir("e2e");
    fs::create_dir_all(&plugins_dir).unwrap();
    let staged = stage_modules(&plugins_dir);
    let (_registry, coordinator) = env(&plugins_dir);

    for (id, module_dir) in &staged {
        let m: Manifest =
            serde_json::from_str(&fs::read_to_string(module_dir.join("plugin.json")).unwrap())
                .unwrap();
        let fixture =
            pick_fixture(module_dir, &m).unwrap_or_else(|| panic!("模块 {id} 无匹配 fixtures"));
        eprintln!("[..] {id}: 导入 {}", fixture.display());

        let outcome = coordinator.import_with_plugin(fixture.clone(), id).await;
        assert_eq!(
            outcome.status,
            ImportStatus::Ready,
            "模块 {id} 导入应 Ready, outcome: {outcome:?}"
        );
        let file_id = outcome.file_id.expect("Ready 有 file_id");

        let metrics = coordinator.store().metrics_of(&file_id);
        assert!(
            !metrics.is_empty(),
            "模块 {id} 应产出至少 1 个指标, got: {metrics:?}"
        );
        eprintln!("[ok] {id}: Ready，指标 {} 个: {metrics:?}", metrics.len());

        // 序列查询：数据域内。
        let range = coordinator
            .store()
            .time_range(&file_id)
            .expect("time_range");
        let composite: Vec<String> = metrics
            .iter()
            .map(|m| format!("{file_id}:{id}:{m}"))
            .collect();
        let slices = query_series_logic(
            &coordinator,
            std::slice::from_ref(&file_id),
            &composite,
            range.start_ms,
            range.end_ms,
            2000,
        )
        .expect("query_series 不应 reject");
        assert!(
            !slices.is_empty() && slices.iter().any(|s| s.point_count > 0),
            "模块 {id} 序列应有点, slices: {slices:?}"
        );
        eprintln!(
            "[ok] {id}: query_series {} 个切片，总点数 {}",
            slices.len(),
            slices.iter().map(|s| s.point_count).sum::<usize>()
        );

        // key_values：数据域中点。
        let kv: Vec<ab_app::commands::query::KeyValueResultDto> =
            key_values_at_logic(&coordinator, std::slice::from_ref(&file_id), range.start_ms)
                .await
                .expect("key_values 整体永不 reject");
        let kv_file = kv.iter().find(|r| r.file_id == file_id).expect("kv 行");
        assert!(
            kv_file.error.is_none(),
            "模块 {id} key_values 不应报错: {:?}",
            kv_file.error
        );
        eprintln!(
            "[ok] {id}: key_values {} 条 entries",
            kv_file.entries.as_ref().map(|e| e.len()).unwrap_or(0)
        );
    }
}

/// 测试 3：模块管理闭环——ZIP 安装 → 同版本冲突语义 → 禁用 → 启用 → 卸载。
#[tokio::test]
#[ignore]
async fn module_manager_roundtrip_on_user_modules() {
    let sources = module_sources();
    if sources.is_empty() {
        eprintln!("SKIP: AB_E2E_USER_MODULES 未设置");
        return;
    }
    let plugins_dir = unique_dir("mgr");
    fs::create_dir_all(&plugins_dir).unwrap();
    let staged = stage_modules(&plugins_dir);
    let (registry, coordinator) = env(&plugins_dir);
    let meta = PluginMeta::new();

    for (id, module_dir) in &staged {
        // 已存在同版本目录 → 安装应报 module_conflict 且 data.kind == same_version。
        let zip_path = zip_of(module_dir);
        let conflict =
            install_plugin_zip_logic(&coordinator, &registry, &plugins_dir, &zip_path, false)
                .await
                .expect_err("同版本安装应冲突");
        assert_eq!(conflict.code, "module_conflict", "{conflict:?}");
        assert_eq!(
            conflict
                .data
                .as_ref()
                .and_then(|d| d.get("kind"))
                .and_then(|k| k.as_str()),
            Some("same_version"),
            "data 应标 same_version: {conflict:?}"
        );
        eprintln!("[ok] {id}: 同版本冲突语义正确（data.kind=same_version）");

        // 禁用 → 列表含 disabled=true 行。
        set_plugin_enabled_logic(&registry, &plugins_dir, id, false)
            .await
            .expect("禁用成功");
        let listed = list_plugins_logic(&registry, &meta, &coordinator, &plugins_dir);
        let row = listed.iter().find(|p| p.id == *id).expect("禁用后仍在列表");
        assert!(row.disabled, "禁用后 disabled=true");
        eprintln!("[ok] {id}: 禁用后列表可见 disabled=true");

        // 启用 → 恢复发现。
        set_plugin_enabled_logic(&registry, &plugins_dir, id, true)
            .await
            .expect("启用成功");
        let listed = list_plugins_logic(&registry, &meta, &coordinator, &plugins_dir);
        let row = listed.iter().find(|p| p.id == *id).expect("启用后仍在列表");
        assert!(!row.disabled, "启用后 disabled=false");
        eprintln!("[ok] {id}: 启用恢复");

        // 卸载 → 目录消失 + 列表无该行。
        uninstall_plugin_logic(&coordinator, &registry, &plugins_dir, id)
            .await
            .expect("卸载成功");
        assert!(
            !plugins_dir.join(id).exists(),
            "卸载后目录应删除: {}",
            plugins_dir.join(id).display()
        );
        let listed = list_plugins_logic(&registry, &meta, &coordinator, &plugins_dir);
        assert!(
            !listed.iter().any(|p| p.id == *id),
            "卸载后列表不应含 {id}: {listed:?}"
        );
        eprintln!("[ok] {id}: 卸载完成，目录与列表均清除");
    }
}

// ---------- 小工具 ----------

/// 把模块目录打成 ZIP（Stored，插件根 = 根目录 → 安装管线接受）。
fn zip_of(module_dir: &Path) -> String {
    let zip_path = unique_dir("zip").join("module.zip");
    fs::create_dir_all(zip_path.parent().expect("zip parent")).expect("mkdir zip");
    let file = File::create(&zip_path).expect("create zip");
    let mut zw = ZipWriter::new(file);
    let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let mut stack = vec![(module_dir.to_path_buf(), String::new())];
    while let Some((dir, prefix)) = stack.pop() {
        for entry in fs::read_dir(&dir).expect("read dir") {
            let entry = entry.expect("entry");
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == ".git"
                || name == ".pytest_cache"
                || name == "__pycache__"
                || name == "fixtures"
            {
                continue;
            }
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            if entry.file_type().expect("ft").is_dir() {
                stack.push((entry.path(), rel));
            } else {
                zw.start_file(&rel, opts).expect("start file");
                let bytes = fs::read(entry.path()).expect("read file");
                zw.write_all(&bytes).expect("write file");
            }
        }
    }
    zw.finish().expect("finish zip");
    zip_path.to_string_lossy().into_owned()
}
