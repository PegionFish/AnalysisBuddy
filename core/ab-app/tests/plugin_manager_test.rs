//! 模块管理命令集成测试（任务 5）：ZIP 安装管线 / 卸载 / 禁用启用 +
//! 状态文件（spec §4.2 / §4.4 / §3.2 / §5.1 错误码）。
//!
//! fixture ZIP 全部测试内用 `zip` crate 现场生成（Stored 无压缩），
//! 复用 host_query_chain_test 的 coordinator 构造模式（真实 registry +
//! 预注册 MockSession）。安装校验与发现扫描同函数（load_manifest /
//! validate / resolve_entry），故每个 good-zip 必须含 command 指向的
//! 入口文件（`./run.exe`）。

mod common;

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ab_host::{PluginRegistry, PluginRuntime};
use ab_pipeline::mock::{FileFixture, MockSession, ParseStep, SessionFixture};
use ab_pipeline::{SessionRegistry, Store};
use ab_protocol::types::{
    Aggregation, FileSummary, MetricDef, Record, RecordBatch, SchemaResult, TimeRange,
};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use ab_app::commands::plugin::{list_plugins_logic, reload_plugin_logic};
use ab_app::commands::plugin_manager::{
    extract_plugin_zip, install_plugin_zip_logic, load_module_state, save_module_state,
    set_plugin_enabled_logic, uninstall_plugin_logic, ZipError,
};
use ab_app::events::PluginMeta;
use ab_app::pipeline_bridge::{ImportCoordinator, ImportStatus, PipelineConfig};

use common::{happy_script, mock_plugin_bin};

/// 数据域：2026-08-01T00:00:00Z（UTC 毫秒），与 host_query_chain_test 同域。
const T_BASE_MS: i64 = 1_785_542_400_000;

/// 每测试独立临时目录（同进程多测试并行互不干扰：pid + 进程内递增序号 +
/// 纳秒时间戳）。
static NEXT_TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn unique_dir() -> PathBuf {
    let seq = NEXT_TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "ab-plugin-mgr-{}-{seq}-{nanos}",
        std::process::id()
    ))
}

/// 测试环境：真实三源 registry（三源同路径 = 临时 plugins 目录）+ 空 coordinator。
struct Env {
    plugins_dir: PathBuf,
    registry: Arc<PluginRegistry>,
    coordinator: ImportCoordinator,
    meta: PluginMeta,
}

fn env() -> Env {
    let plugins_dir = unique_dir();
    fs::create_dir_all(&plugins_dir).expect("mkdir plugins dir");
    let registry = Arc::new(PluginRegistry::with_sources(
        plugins_dir.clone(),
        plugins_dir.clone(),
        plugins_dir.clone(),
    ));
    let coordinator = ImportCoordinator::new(
        Arc::new(Store::new()),
        Arc::new(SessionRegistry::new()),
        tokio::sync::mpsc::unbounded_channel().0,
        Arc::new(PluginRuntime::new(registry.clone())),
        registry.clone(),
    );
    Env {
        plugins_dir,
        registry,
        coordinator,
        meta: PluginMeta::new(),
    }
}

/// 现场生成 fixture ZIP（条目名 → 内容；Stored 无压缩，测试确定性）。
fn build_zip(path: &Path, entries: &[(&str, &str)]) {
    let bytes: Vec<(&str, &[u8])> = entries
        .iter()
        .map(|(name, content)| (*name, content.as_bytes()))
        .collect();
    build_zip_bytes(path, &bytes);
}

/// 二进制条目版本（真实可执行文件打包用）。
fn build_zip_bytes(path: &Path, entries: &[(&str, &[u8])]) {
    let file = File::create(path).expect("create zip");
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default();
    for (name, content) in entries {
        zip.start_file(*name, options).expect("start entry");
        zip.write_all(content).expect("write entry");
    }
    zip.finish().expect("finish zip");
}

/// `n` 个微小条目的 ZIP（条目数限额测试用）。
fn build_n_entry_zip(path: &Path, n: usize) {
    let file = File::create(path).expect("create zip");
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default();
    for i in 0..n {
        zip.start_file(format!("f{i:04}.txt"), options)
            .expect("start entry");
        zip.write_all(b"x").expect("write entry");
    }
    zip.finish().expect("finish zip");
}

/// 每条目声明解压尺寸为 `sizes[i]` 的 ZIP（Deflate 压缩：内容全零，
/// ZIP 本体极小，但中心目录声明的 uncompressed size 逐项精确）。
fn write_deflated_zeros_zip(path: &Path, sizes: &[u64]) {
    let file = File::create(path).expect("create zip");
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let chunk = vec![0u8; 1024 * 1024];
    for (i, size) in sizes.iter().enumerate() {
        zip.start_file(format!("big{i}.bin"), options)
            .expect("start entry");
        let mut remaining = *size;
        while remaining > 0 {
            let n = remaining.min(chunk.len() as u64) as usize;
            zip.write_all(&chunk[..n]).expect("write entry");
            remaining -= n as u64;
        }
    }
    zip.finish().expect("finish zip");
}

/// 合法 manifest（含可选元信息 update_url/author/repository/tools/changelog，
/// 断言 DTO 透传）。
fn manifest_json(id: &str, version: &str) -> String {
    format!(
        r#"{{
  "id": "{id}",
  "display_name": "Install Test {id}",
  "version": "{version}",
  "author": "PegionFish",
  "repository": "https://github.com/owner/repo",
  "tools": ["AnalysisBuddy >= 0.1.0"],
  "update_url": "https://github.com/owner/repo",
  "changelog": [
    {{ "version": "{version}", "date": "2026-08-01", "notes": ["新增"] }}
  ],
  "entry": {{ "command": "./run.exe", "args": [] }},
  "match": {{ "extensions": ["csv"], "header_fingerprints": null }},
  "min_protocol_version": 1
}}"#
    )
}

/// good-zip：根 plugin.json + 入口文件（resolve_entry 需要 command 目标存在）。
fn good_zip(path: &Path, id: &str, version: &str) {
    build_zip(
        path,
        &[
            ("plugin.json", &manifest_json(id, version)),
            ("run.exe", "MZ"),
        ],
    );
}

/// 安装 helper（失败即 panic 并带错误码）。
async fn install(env: &Env, id: &str, version: &str) {
    let zip = env.plugins_dir.join(format!("{id}-{version}.zip"));
    good_zip(&zip, id, version);
    install_plugin_zip_logic(
        &env.coordinator,
        &env.registry,
        &env.plugins_dir,
        &zip.display().to_string(),
        false,
    )
    .await
    .unwrap_or_else(|e| panic!("install {id} v{version} 失败：{}（{}）", e.code, e.message));
    assert!(
        env.plugins_dir.join(id).join("plugin.json").is_file(),
        "安装后目录必须含 plugin.json"
    );
}

/// 安装失败后不得残留 `.tmp-*` 解压目录（临时目录必须清理；已安装的
/// 插件目录与 fixture ZIP 文件不计）。
fn assert_no_tmp_leftovers(dir: &Path) {
    let dirs: Vec<String> = fs::read_dir(dir)
        .expect("read plugins dir")
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".tmp"))
        .collect();
    assert!(dirs.is_empty(), "失败路径不得残留临时目录：{dirs:?}");
}

// ---------------------------------------------------------------------------
// 安装管线（spec §4.2 七步）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn install_good_zip_creates_plugin_dir_and_returns_dto() {
    let e = env();
    let zip = e.plugins_dir.join("good.zip");
    good_zip(&zip, "install-test", "1.0.0");

    let dto = install_plugin_zip_logic(
        &e.coordinator,
        &e.registry,
        &e.plugins_dir,
        &zip.display().to_string(),
        false,
    )
    .await
    .expect("install");

    assert_eq!(dto.id, "install-test");
    assert_eq!(dto.version, "1.0.0");
    assert!(!dto.builtin, "非内建模块 builtin 必须为 false");
    assert_eq!(dto.source, "portable", "安装目标为 portable 源");
    assert!(!dto.disabled, "新装模块不应处于禁用状态");
    assert_eq!(
        dto.update_url.as_deref(),
        Some("https://github.com/owner/repo"),
        "manifest.update_url 必须透传 DTO"
    );
    assert_eq!(
        dto.author.as_deref(),
        Some("PegionFish"),
        "manifest.author 必须透传 DTO"
    );
    assert_eq!(
        dto.repository.as_deref(),
        Some("https://github.com/owner/repo"),
        "manifest.repository 必须透传 DTO"
    );
    assert_eq!(
        dto.tools.as_deref(),
        Some(&["AnalysisBuddy >= 0.1.0".to_string()][..]),
        "manifest.tools 必须透传 DTO"
    );
    assert_eq!(
        dto.changelog.as_ref().map(|c| c.len()),
        Some(1),
        "manifest.changelog 必须透传 DTO"
    );
    let dir = e.plugins_dir.join("install-test");
    assert!(dir.join("plugin.json").is_file(), "目录必须含 plugin.json");
    assert!(dir.join("run.exe").is_file(), "数据文件必须解压");
    assert!(
        e.registry
            .list()
            .iter()
            .any(|p| p.manifest.id == "install-test"),
        "reload 后必须在发现列表"
    );
    let _ = fs::remove_dir_all(&e.plugins_dir);
}

#[tokio::test]
async fn install_zip_without_plugin_json_rejected() {
    let e = env();
    let zip = e.plugins_dir.join("no-manifest.zip");
    build_zip(&zip, &[("run.exe", "MZ")]);

    let err = install_plugin_zip_logic(
        &e.coordinator,
        &e.registry,
        &e.plugins_dir,
        &zip.display().to_string(),
        false,
    )
    .await
    .expect_err("缺 plugin.json 必须拒绝");

    assert_eq!(err.code, "module_install");
    assert!(!e.plugins_dir.join("install-test").exists());
    assert_no_tmp_leftovers(&e.plugins_dir);
    let _ = fs::remove_dir_all(&e.plugins_dir);
}

#[tokio::test]
async fn install_zip_slip_entry_rejected() {
    let e = env();
    let zip = e.plugins_dir.join("slip.zip");
    build_zip(&zip, &[("../evil.txt", "x")]);

    let err = install_plugin_zip_logic(
        &e.coordinator,
        &e.registry,
        &e.plugins_dir,
        &zip.display().to_string(),
        false,
    )
    .await
    .expect_err("zip-slip 条目必须拒绝");

    assert_eq!(err.code, "module_install");
    assert!(
        err.message.contains("evil.txt"),
        "错误信息应点名越界条目：{}",
        err.message
    );
    assert!(
        !e.plugins_dir.parent().unwrap().join("evil.txt").exists(),
        "越界文件不得写出目标目录"
    );
    assert_no_tmp_leftovers(&e.plugins_dir);
    let _ = fs::remove_dir_all(&e.plugins_dir);
}

#[tokio::test]
async fn install_zip_with_extra_top_level_dir_accepted() {
    let e = env();
    let zip = e.plugins_dir.join("extra-dir.zip");
    build_zip(
        &zip,
        &[
            ("plugin.json", &manifest_json("install-test", "1.0.0")),
            ("run.exe", "MZ"),
            ("extra-dir/data.txt", "hello"),
        ],
    );

    install_plugin_zip_logic(
        &e.coordinator,
        &e.registry,
        &e.plugins_dir,
        &zip.display().to_string(),
        false,
    )
    .await
    .expect("根为基准：额外顶层目录可接受");

    assert!(
        e.plugins_dir
            .join("install-test/extra-dir/data.txt")
            .is_file(),
        "额外顶层目录内容必须原样解压"
    );
    let _ = fs::remove_dir_all(&e.plugins_dir);
}

#[tokio::test]
async fn install_same_id_same_version_conflicts() {
    let e = env();
    install(&e, "install-test", "1.0.0").await;
    let zip = e.plugins_dir.join("same.zip");
    good_zip(&zip, "install-test", "1.0.0");

    let err = install_plugin_zip_logic(
        &e.coordinator,
        &e.registry,
        &e.plugins_dir,
        &zip.display().to_string(),
        false,
    )
    .await
    .expect_err("同 id 同版本必须按「已安装」拒绝");

    assert_eq!(err.code, "module_conflict");
    assert!(
        err.message.contains("already installed"),
        "已安装语义：{}",
        err.message
    );
    assert_no_tmp_leftovers(&e.plugins_dir);
    let _ = fs::remove_dir_all(&e.plugins_dir);
}

#[tokio::test]
async fn install_same_id_different_version_needs_overwrite() {
    let e = env();
    install(&e, "install-test", "1.0.0").await;

    let zip = e.plugins_dir.join("v2.zip");
    good_zip(&zip, "install-test", "2.0.0");
    let err = install_plugin_zip_logic(
        &e.coordinator,
        &e.registry,
        &e.plugins_dir,
        &zip.display().to_string(),
        false,
    )
    .await
    .expect_err("不同版本 overwrite=false 必须冲突");

    assert_eq!(err.code, "module_conflict");
    assert!(
        e.plugins_dir.join("install-test/plugin.json").is_file(),
        "冲突失败不得触碰既有安装"
    );

    let dto = install_plugin_zip_logic(
        &e.coordinator,
        &e.registry,
        &e.plugins_dir,
        &zip.display().to_string(),
        true,
    )
    .await
    .expect("overwrite=true 覆盖成功");

    assert_eq!(dto.version, "2.0.0");
    let json = fs::read_to_string(e.plugins_dir.join("install-test/plugin.json"))
        .expect("read installed manifest");
    assert!(
        json.contains("\"version\": \"2.0.0\""),
        "目录内 manifest 版本必须更新"
    );
    assert!(
        e.registry
            .get("install-test")
            .map(|p| p.manifest.version == "2.0.0")
            .unwrap_or(false),
        "reload 后发现的版本必须为新版"
    );
    let _ = fs::remove_dir_all(&e.plugins_dir);
}

#[tokio::test]
async fn install_builtin_id_rejected_as_protected() {
    let e = env();
    let zip = e.plugins_dir.join("builtin.zip");
    good_zip(&zip, "builtin-csv", "1.0.0");

    let err = install_plugin_zip_logic(
        &e.coordinator,
        &e.registry,
        &e.plugins_dir,
        &zip.display().to_string(),
        true,
    )
    .await
    .expect_err("内建 id 即使 overwrite=true 也必须拒绝");

    assert_eq!(err.code, "module_protected");
    assert_no_tmp_leftovers(&e.plugins_dir);
    let _ = fs::remove_dir_all(&e.plugins_dir);
}

// ---------------------------------------------------------------------------
// 限额（spec §5.2：≤100MB、≤2000 条目）+ 解压膨胀防护
// ---------------------------------------------------------------------------

#[tokio::test]
async fn install_rejects_zip_over_100mb() {
    let e = env();
    let zip = e.plugins_dir.join("huge.zip");
    // 稀疏文件：>100MB 即拒（检查发生在解压前，无需真实内容）。
    File::create(&zip)
        .expect("create sparse zip")
        .set_len(100 * 1024 * 1024 + 1)
        .expect("set sparse length");

    let err = install_plugin_zip_logic(
        &e.coordinator,
        &e.registry,
        &e.plugins_dir,
        &zip.display().to_string(),
        false,
    )
    .await
    .expect_err(">100MB ZIP 必须拒绝");

    assert_eq!(err.code, "module_install");
    assert!(
        err.message.contains("exceeds limit"),
        "错误信息应点名限额：{}",
        err.message
    );
    assert_no_tmp_leftovers(&e.plugins_dir);
    let _ = fs::remove_dir_all(&e.plugins_dir);
}

#[tokio::test]
async fn install_rejects_zip_over_2000_entries() {
    let e = env();
    let zip = e.plugins_dir.join("many.zip");
    build_n_entry_zip(&zip, 2001);

    let err = install_plugin_zip_logic(
        &e.coordinator,
        &e.registry,
        &e.plugins_dir,
        &zip.display().to_string(),
        false,
    )
    .await
    .expect_err(">2000 条目 ZIP 必须拒绝");

    assert_eq!(err.code, "module_install");
    assert!(
        err.message.contains("2000"),
        "错误信息应点名条目数上限：{}",
        err.message
    );
    assert_no_tmp_leftovers(&e.plugins_dir);
    let _ = fs::remove_dir_all(&e.plugins_dir);
}

#[test]
fn extract_rejects_entry_declaring_over_500mb_unpacked() {
    let dir = unique_dir();
    fs::create_dir_all(&dir).expect("mkdir");
    let zip = dir.join("big-entry.zip");
    let dest = dir.join("dest");
    // 单条目声明解压 600MiB（Deflate 压缩全零，ZIP 本体极小）：per-entry
    // 上限在写盘前按中心目录 entry.size() 判定。
    write_deflated_zeros_zip(&zip, &[600 * 1024 * 1024]);

    let err = extract_plugin_zip(&zip, &dest).expect_err("声明 >500MiB 的单条目必须拒绝");

    assert!(matches!(err, ZipError::Limits(_)), "应为限额类错误：{err}");
    assert!(
        err.to_string().contains("unpacked entry"),
        "错误信息应点名解压尺寸超限条目：{err}"
    );
    assert!(
        !dest.join("big0.bin").exists(),
        "超限条目本身不得写盘（写盘前按 entry.size() 判定）"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn extract_rejects_total_unpacked_over_1gb() {
    let dir = unique_dir();
    fs::create_dir_all(&dir).expect("mkdir");
    let zip = dir.join("many-big.zip");
    let dest = dir.join("dest");
    // 三条 350MiB：单条 < 500MiB（per-entry 通过），合计 1.05GiB > 1GiB
    // （total 上限在第 3 条累积处触发）。
    write_deflated_zeros_zip(
        &zip,
        &[350 * 1024 * 1024, 350 * 1024 * 1024, 350 * 1024 * 1024],
    );

    let err = extract_plugin_zip(&zip, &dest).expect_err("合计声明 >1GiB 必须拒绝");

    assert!(matches!(err, ZipError::Limits(_)), "应为限额类错误：{err}");
    assert!(
        err.to_string().contains("total unpacked"),
        "错误信息应点名累计解压尺寸超限：{err}"
    );
    assert!(
        !dest.join("big2.bin").exists(),
        "触发累计上限的条目不得写盘"
    );
    let _ = fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// 卸载（spec §4.4：关闭全部会话 → 删目录 → reload）
// ---------------------------------------------------------------------------

/// 脚本化会话：schema 声明 `fps`，parse 吐 3 点（2026-08-01 域）。
fn scripted_session(csv_path_str: &str, plugin_id: &str) -> Arc<MockSession> {
    let records: Vec<Record> = (0..3)
        .map(|i| Record {
            timestamp: T_BASE_MS + i * 1_000,
            metric: "fps".to_string(),
            value: 60.0 - i as f64,
            level: None,
            tags: None,
            raw_line: None,
        })
        .collect();
    let batch = RecordBatch {
        file_id: String::new(),
        seq: 0,
        records,
        done: true,
    };
    let mut files = HashMap::new();
    files.insert(
        csv_path_str.to_string(),
        FileFixture {
            load_file: Some(Ok(FileSummary {
                record_count_hint: Some(3),
                time_range: Some(TimeRange {
                    start_ms: T_BASE_MS,
                    end_ms: T_BASE_MS + 2_000,
                }),
                note: None,
            })),
            parse_script: vec![ParseStep::Batch(batch)],
            parse_result: None,
            key_values: None,
        },
    );
    MockSession::new(SessionFixture {
        plugin_id: plugin_id.to_string(),
        schema: Some(Ok(SchemaResult {
            metrics: vec![MetricDef {
                id: "fps".to_string(),
                name: "Frames per second".to_string(),
                unit: Some("Hz".to_string()),
                description: None,
                aggregation: Aggregation::Last,
            }],
        })),
        can_handle: None,
        files,
    })
}

/// fixture 真实路径（导入编排会读取文件元信息，路径必须存在）。
fn fixture_csv() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/small_with_header.csv");
    assert!(path.exists(), "缺少测试 fixture：{}", path.display());
    path
}

#[tokio::test]
async fn uninstall_closes_files_and_removes_dir() {
    let e = env();
    install(&e, "uninstall-test", "1.0.0").await;

    // 真实导入一条文件（预注册 MockSession 命中，无需进程）→ Ready。
    let csv = fixture_csv();
    let csv_str = csv.display().to_string();
    let session = scripted_session(&csv_str, "uninstall-test");
    e.coordinator.registry().register(session);
    let outcome = e
        .coordinator
        .import_with_plugin(csv, "uninstall-test")
        .await;
    assert_eq!(outcome.status, ImportStatus::Ready, "导入应 Ready");
    let file_id = outcome.file_id.expect("Ready 必带 file_id");
    assert!(e.coordinator.list_frozen().contains(&file_id));

    uninstall_plugin_logic(
        &e.coordinator,
        &e.registry,
        &e.plugins_dir,
        "uninstall-test",
    )
    .await
    .expect("uninstall");

    assert!(
        !e.plugins_dir.join("uninstall-test").exists(),
        "卸载后目录必须删除"
    );
    assert!(
        !e.registry
            .list()
            .iter()
            .any(|p| p.manifest.id == "uninstall-test"),
        "reload 后从发现列表消失"
    );
    assert!(
        e.coordinator.list_frozen().is_empty(),
        "卸载前必须关闭该插件全部文件会话"
    );
    assert!(
        e.coordinator
            .file_index()
            .files_of("uninstall-test")
            .is_empty(),
        "file_index 驻留映射必须清空"
    );
    let _ = fs::remove_dir_all(&e.plugins_dir);
}

#[tokio::test]
async fn uninstall_builtin_or_unknown_rejected() {
    let e = env();

    let err = uninstall_plugin_logic(&e.coordinator, &e.registry, &e.plugins_dir, "builtin-csv")
        .await
        .expect_err("内建模块不可卸载");
    assert_eq!(err.code, "module_protected");

    let err = uninstall_plugin_logic(&e.coordinator, &e.registry, &e.plugins_dir, "ghost")
        .await
        .expect_err("未知模块");
    assert_eq!(err.code, "module_not_found");
    let _ = fs::remove_dir_all(&e.plugins_dir);
}

/// 卸载必须终止插件进程：真实 mock-plugin 进程（CWD = 插件目录）常驻时，
/// 删目录会被进程句柄占用 → `module_in_use`；修复后进程被 shutdown，
/// 目录可立即删除。
#[tokio::test]
async fn uninstall_terminates_live_plugin_process_and_removes_dir() {
    const LIVE_FILE_ID: &str = "f2c1d2a4-9e7b-4a01-b2c3-0d5e6f7a8b9c";
    const PLUGIN_ID: &str = "uninstall-live";

    // 定制 env：固定 file_id（剧本内嵌 file_id 须与协调器一致）。
    let plugins_dir = unique_dir();
    fs::create_dir_all(&plugins_dir).expect("mkdir plugins dir");
    let registry = Arc::new(PluginRegistry::with_sources(
        plugins_dir.clone(),
        plugins_dir.clone(),
        plugins_dir.clone(),
    ));
    let config = PipelineConfig {
        file_id_fn: Some(Arc::new(|_| LIVE_FILE_ID.to_string())),
        ..PipelineConfig::default()
    };
    let coordinator = ImportCoordinator::with_config(
        Arc::new(Store::new()),
        Arc::new(SessionRegistry::new()),
        tokio::sync::mpsc::unbounded_channel().0,
        Arc::new(PluginRuntime::new(registry.clone())),
        registry.clone(),
        config,
    );

    // 剧本（真实进程可执行，initialize id 须与插件 id 一致）。
    let script_path = plugins_dir.join("live.ndjson");
    fs::write(&script_path, happy_script(PLUGIN_ID, LIVE_FILE_ID)).expect("write script");

    // ZIP 安装：入口 = 打包进插件的 mock-plugin.exe + `--script <剧本>`。
    let exe = mock_plugin_bin();
    let manifest = serde_json::json!({
        "id": PLUGIN_ID,
        "display_name": "Live Process Plugin",
        "version": "1.0.0",
        "entry": {
            "command": "./mock-plugin.exe",
            "args": ["--script", script_path.to_string_lossy().into_owned()]
        },
        "match": { "extensions": ["csv"], "header_fingerprints": null },
        "min_protocol_version": 1
    });
    let zip_path = plugins_dir.join("live.zip");
    build_zip_bytes(
        &zip_path,
        &[
            (
                "plugin.json",
                serde_json::to_string_pretty(&manifest)
                    .expect("serialize manifest")
                    .as_bytes(),
            ),
            (
                "mock-plugin.exe",
                &fs::read(&exe).expect("read mock-plugin exe"),
            ),
        ],
    );
    install_plugin_zip_logic(
        &coordinator,
        &registry,
        &plugins_dir,
        &zip_path.display().to_string(),
        false,
    )
    .await
    .expect("install");

    // 真实进程导入 → Ready（进程常驻，CWD = 插件目录）。
    let outcome = coordinator
        .import_with_plugin(fixture_csv(), PLUGIN_ID)
        .await;
    assert_eq!(
        outcome.status,
        ImportStatus::Ready,
        "真实进程导入应 Ready：{outcome:?}"
    );
    assert_eq!(outcome.file_id.as_deref(), Some(LIVE_FILE_ID));

    // 卸载：必须终止进程后立即可删目录。
    uninstall_plugin_logic(&coordinator, &registry, &plugins_dir, PLUGIN_ID)
        .await
        .expect("卸载必须终止插件进程并删除目录");
    assert!(
        !plugins_dir.join(PLUGIN_ID).exists(),
        "卸载后目录必须已删除（进程已终止，无 CWD 句柄占用）"
    );
    let _ = fs::remove_dir_all(&plugins_dir);
}

// ---------------------------------------------------------------------------
// 禁用/启用（spec §4.4 + §3.2 状态文件）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn disable_hides_plugin_and_reload_rejects_then_enable_restores() {
    let e = env();
    install(&e, "install-test", "1.0.0").await;

    set_plugin_enabled_logic(&e.registry, &e.plugins_dir, "install-test", false).expect("disable");

    assert!(e.registry.is_disabled("install-test"));
    assert!(
        !e.registry
            .list()
            .iter()
            .any(|p| p.manifest.id == "install-test"),
        "禁用后从发现列表消失"
    );
    let state = load_module_state(&e.plugins_dir);
    assert!(state.contains("install-test"), "状态文件必须持久化禁用 id");

    // §4.4：禁用模块 reload 拒绝（复用 §5.1 module_not_found）。
    let err = reload_plugin_logic(&e.registry, &e.meta, &e.coordinator, "install-test")
        .await
        .expect_err("禁用模块不可重建");
    assert_eq!(err.code, "module_not_found");
    assert!(
        err.message.contains("disabled"),
        "错误信息应点名禁用：{}",
        err.message
    );

    set_plugin_enabled_logic(&e.registry, &e.plugins_dir, "install-test", true).expect("enable");

    assert!(!e.registry.is_disabled("install-test"));
    assert!(
        e.registry
            .list()
            .iter()
            .any(|p| p.manifest.id == "install-test"),
        "启用后回到发现列表"
    );
    assert!(
        load_module_state(&e.plugins_dir).is_empty(),
        "启用后状态文件必须移除该 id"
    );
    let _ = fs::remove_dir_all(&e.plugins_dir);
}

#[tokio::test]
async fn set_plugin_enabled_unknown_id_rejected() {
    let e = env();
    let err = set_plugin_enabled_logic(&e.registry, &e.plugins_dir, "ghost", false)
        .expect_err("未知模块");
    assert_eq!(err.code, "module_not_found");
    let _ = fs::remove_dir_all(&e.plugins_dir);
}

// ---------------------------------------------------------------------------
// list_plugins 禁用合并展示（用户裁定：发现过滤禁用，命令层合并展示，
// UI 才能提供「启用」入口）+ PluginInfoDto 展示字段（author/repository/
// tools/changelog）
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_plugins_merges_disabled_plugins_with_manifest_meta() {
    let e = env();
    install(&e, "install-test", "1.0.0").await;
    set_plugin_enabled_logic(&e.registry, &e.plugins_dir, "install-test", false).expect("disable");

    assert!(
        !e.registry
            .list()
            .iter()
            .any(|p| p.manifest.id == "install-test"),
        "前置：禁用后不在发现列表"
    );

    let list = list_plugins_logic(&e.registry, &e.meta, &e.coordinator, &e.plugins_dir);
    let entry = list
        .iter()
        .find(|p| p.id == "install-test")
        .expect("禁用模块仍必须在列表（供 UI 启用）");
    assert!(entry.disabled, "合并行 disabled=true");
    assert_eq!(entry.source, "portable", "manifest 可读 → portable");
    assert!(!entry.builtin);
    assert_eq!(entry.display_name, "Install Test install-test");
    assert_eq!(entry.version, "1.0.0");
    assert_eq!(
        entry.update_url.as_deref(),
        Some("https://github.com/owner/repo")
    );
    assert_eq!(entry.author.as_deref(), Some("PegionFish"));
    assert_eq!(
        entry.repository.as_deref(),
        Some("https://github.com/owner/repo")
    );
    assert_eq!(
        entry.tools.as_deref(),
        Some(&["AnalysisBuddy >= 0.1.0".to_string()][..])
    );
    assert_eq!(entry.changelog.as_ref().map(|c| c.len()), Some(1));

    // 启用后回到发现列表（disabled=false），且不重复出现。
    set_plugin_enabled_logic(&e.registry, &e.plugins_dir, "install-test", true).expect("enable");
    let list = list_plugins_logic(&e.registry, &e.meta, &e.coordinator, &e.plugins_dir);
    assert_eq!(
        list.iter().filter(|p| p.id == "install-test").count(),
        1,
        "发现列表与合并展示不得重复"
    );
    assert!(
        !list
            .iter()
            .find(|p| p.id == "install-test")
            .expect("存在")
            .disabled
    );
    let _ = fs::remove_dir_all(&e.plugins_dir);
}

#[tokio::test]
async fn list_plugins_marks_disabled_plugin_with_corrupt_manifest_invalid() {
    let e = env();
    install(&e, "install-test", "1.0.0").await;
    fs::write(e.plugins_dir.join("install-test/plugin.json"), "{ not json")
        .expect("corrupt manifest");
    set_plugin_enabled_logic(&e.registry, &e.plugins_dir, "install-test", false).expect("disable");

    let list = list_plugins_logic(&e.registry, &e.meta, &e.coordinator, &e.plugins_dir);
    let entry = list
        .iter()
        .find(|p| p.id == "install-test")
        .expect("禁用模块仍在列表（manifest 损坏按 invalid 展示）");
    assert!(entry.disabled);
    assert_eq!(entry.source, "invalid", "manifest 不可读 → invalid 保留值");
    assert_eq!(entry.display_name, "install-test", "display_name 回落 id");
    assert_eq!(entry.version, "", "manifest 不可读 → 版本为空");
    assert_eq!(entry.update_url, None);
    assert_eq!(entry.author, None);
    let _ = fs::remove_dir_all(&e.plugins_dir);
}

// ---------------------------------------------------------------------------
// 状态文件（spec §3.2：损坏回退空集 + 原子写）
// ---------------------------------------------------------------------------

#[test]
fn module_state_roundtrip_and_corruption_fallback() {
    let dir = unique_dir();
    fs::create_dir_all(&dir).expect("mkdir");

    assert!(load_module_state(&dir).is_empty(), "缺失 → 空集");

    let set: HashSet<String> = ["a".to_string(), "b".to_string()].into_iter().collect();
    save_module_state(&dir, &set).expect("save");
    assert_eq!(load_module_state(&dir), set, "往返一致");

    let raw = fs::read_to_string(dir.join(".ab-modules.json")).expect("read state file");
    assert!(
        raw.contains("\"disabled\""),
        "状态文件形状必须为 {{ \"disabled\": [...] }}"
    );

    fs::write(dir.join(".ab-modules.json"), "{ not json").expect("corrupt");
    assert!(
        load_module_state(&dir).is_empty(),
        "损坏 → 空集（eprintln 告警）"
    );

    fs::remove_dir_all(&dir).expect("cleanup");
}
