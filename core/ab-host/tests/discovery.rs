//! A-01 三源发现与 manifest 校验集成测试（host-runtime.md §1/§2 DoD）。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use ab_host::{DiscoveryError, PluginRegistry, PluginSource, ShadowedPlugin};
use ab_protocol::manifest::{Manifest, MatchRules, PluginEntry};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// 极简临时目录 fixture（测试结束自动清理）。
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!(
            "ab-host-{}-{}-{tag}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&base).expect("create tempdir");
        Self(base)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn join(&self, rel: &str) -> PathBuf {
        self.0.join(rel)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn canonical(p: &Path) -> PathBuf {
    let c = p.canonicalize().expect("canonicalize path");
    strip_verbatim_prefix(c)
}

/// 与生产行为对齐（manifest.rs `simplify_canonical`）：Windows 上
/// `canonicalize` 产出的 `\\?\` / `\\?\UNC\` 前缀在解析入口时被剥离。
#[cfg(windows)]
fn strip_verbatim_prefix(path: PathBuf) -> PathBuf {
    use std::path::{Component, Prefix};
    let mut comps = path.components();
    let prefix = match comps.next() {
        Some(Component::Prefix(pc)) => pc,
        _ => return path,
    };
    let rebuilt = match prefix.kind() {
        Prefix::VerbatimDisk(d) => PathBuf::from(format!("{}:\\", d as char)),
        Prefix::VerbatimUNC(server, share) => PathBuf::from(format!(
            "\\\\{}\\{}",
            server.to_string_lossy(),
            share.to_string_lossy()
        )),
        _ => return path,
    };
    let tail = comps.as_path().to_string_lossy().to_string();
    let tail = tail.strip_prefix('\\').unwrap_or(&tail);
    rebuilt.join(tail)
}

#[cfg(not(windows))]
fn strip_verbatim_prefix(path: PathBuf) -> PathBuf {
    path
}

/// 构造一个指向 `dir` 内 `bin/run.exe` 的合法 manifest。
fn manifest(id: &str) -> Manifest {
    Manifest {
        id: id.to_string(),
        display_name: "Test Plugin".to_string(),
        version: "0.1.0".to_string(),
        entry: PluginEntry {
            command: "bin/run.exe".to_string(),
            args: vec!["--stdio".to_string()],
            working_dir: None,
        },
        r#match: MatchRules {
            extensions: vec!["csv".to_string()],
            header_fingerprints: None,
        },
        min_protocol_version: 1,
        ..Default::default()
    }
}

/// 写入 `plugin.json` 并创建 `bin/run.exe` 占位文件。
fn install_plugin(dir: &Path, m: &Manifest) {
    fs::create_dir_all(dir.join("bin")).expect("create bin dir");
    fs::write(dir.join("bin/run.exe"), b"placeholder").expect("write run.exe");
    fs::write(
        dir.join("plugin.json"),
        serde_json::to_string_pretty(m).expect("serialize manifest"),
    )
    .expect("write plugin.json");
}

fn registry(portable: &Path, user_data: &Path) -> PluginRegistry {
    // install 与 portable 同路径（ZIP 布局）→ 按同一源去重。
    PluginRegistry::with_sources(
        portable.to_path_buf(),
        portable.to_path_buf(),
        user_data.to_path_buf(),
    )
}

#[test]
fn all_three_sources_scanned_and_missing_source_is_empty() {
    let tmp = TempDir::new("three-sources");
    let portable = tmp.join("portable");
    let user = tmp.join("user");
    fs::create_dir_all(portable.join("p1")).expect("mkdir p1");
    install_plugin(&portable.join("p1"), &manifest("p1"));
    // user 源不存在 → 视为空源，不报错、不创建。
    let reg = registry(&portable, &user);
    let outcome = reg.discover();
    assert_eq!(
        outcome.invalid.len(),
        0,
        "missing source must not produce errors"
    );
    assert!(!user.exists(), "missing source dir must not be created");
    assert_eq!(outcome.plugins.len(), 1);
    assert_eq!(outcome.plugins[0].manifest.id, "p1");
    assert_eq!(outcome.plugins[0].source, PluginSource::Portable);

    // UserData 源单独出现也能被发现。
    let tmp2 = TempDir::new("user-only");
    let user2 = tmp2.join("user");
    fs::create_dir_all(user2.join("u1")).expect("mkdir u1");
    install_plugin(&user2.join("u1"), &manifest("u1"));
    let reg = PluginRegistry::with_sources(tmp2.join("portable"), tmp2.join("portable"), user2);
    let outcome = reg.discover();
    assert_eq!(outcome.plugins.len(), 1);
    assert_eq!(outcome.plugins[0].source, PluginSource::UserData);
}

#[test]
fn same_id_conflict_portable_wins_and_event_carries_details() {
    let tmp = TempDir::new("conflict");
    let portable = tmp.join("portable");
    let user = tmp.join("user");
    fs::create_dir_all(portable.join("dup")).expect("mkdir");
    fs::create_dir_all(user.join("dup")).expect("mkdir");
    install_plugin(&portable.join("dup"), &manifest("dup"));
    install_plugin(&user.join("dup"), &manifest("dup"));

    let reg = registry(&portable, &user);
    let mut events = reg.subscribe_events();
    let outcome = reg.reload();

    // Portable 胜出；UserData 落败者进 shadowed。
    assert_eq!(outcome.plugins.len(), 1);
    assert_eq!(outcome.plugins[0].manifest.id, "dup");
    assert_eq!(outcome.plugins[0].source, PluginSource::Portable);
    assert_eq!(outcome.shadowed.len(), 1);
    let ShadowedPlugin {
        id,
        source,
        winner_source,
        ..
    } = &outcome.shadowed[0];
    assert_eq!(id, "dup");
    assert_eq!(*source, PluginSource::UserData);
    assert_eq!(*winner_source, PluginSource::Portable);

    // HostEvent::PluginsReloaded 附带明细。
    let ev = events
        .try_recv()
        .expect("PluginsReloaded event must be published");
    match ev {
        ab_host::HostEvent::PluginsReloaded {
            plugins,
            invalid,
            shadowed,
        } => {
            assert_eq!(plugins.len(), 1);
            assert_eq!(plugins[0].source, PluginSource::Portable);
            assert!(invalid.is_empty());
            assert_eq!(shadowed.len(), 1);
            assert_eq!(shadowed[0].winner_source, PluginSource::Portable);
        }
        other => panic!("expected PluginsReloaded, got {other:?}"),
    }
}

#[test]
fn plugin_with_git_and_unrelated_files_is_discovered() {
    let tmp = TempDir::new("noisy");
    let plugin_dir = tmp.join("noisy-plugin");
    fs::create_dir_all(plugin_dir.join(".git")).expect("mkdir .git");
    fs::create_dir_all(plugin_dir.join("target")).expect("mkdir target");
    fs::create_dir_all(plugin_dir.join("src")).expect("mkdir src");
    fs::write(plugin_dir.join("Cargo.toml"), "[package]").expect("write Cargo.toml");
    fs::write(plugin_dir.join("README.md"), "docs").expect("write README");
    install_plugin(&plugin_dir, &manifest("noisy"));

    let reg = registry(tmp.path(), &tmp.join("user"));
    let outcome = reg.discover();
    assert_eq!(outcome.plugins.len(), 1, "noisy plugin must be found");
    assert_eq!(outcome.invalid.len(), 0, "unrelated files must not error");
}

#[test]
fn nested_layout_is_not_discovered() {
    let tmp = TempDir::new("nested");
    // plugins/a/b/plugin.json —— b 是 a 的孙目录，永不发现。
    fs::create_dir_all(tmp.join("a/b")).expect("mkdir a/b");
    install_plugin(&tmp.join("a/b"), &manifest("deep"));
    fs::write(tmp.join("a/plugin.json"), "{}").expect("write a/plugin.json");

    let reg = registry(tmp.path(), &tmp.join("user"));
    let outcome = reg.discover();
    assert!(
        outcome.plugins.is_empty(),
        "nested layout must not be found"
    );
    // a 有 plugin.json 但内容非法 → 降级为 InvalidPlugin（不中断扫描）。
    let reasons: Vec<&DiscoveryError> = outcome.invalid.iter().map(|i| &i.reason).collect();
    assert!(
        reasons
            .iter()
            .any(|r| matches!(r, DiscoveryError::JsonParse(_))),
        "a must be listed invalid with JsonParse, got {reasons:?}"
    );
}

#[test]
fn invalid_plugins_are_listed_with_reasons() {
    let tmp = TempDir::new("invalid");
    // 缺 plugin.json → MissingManifest
    fs::create_dir_all(tmp.join("no-manifest")).expect("mkdir");
    // id 非法 → InvalidId
    fs::create_dir_all(tmp.join("bad-id")).expect("mkdir");
    let mut bad_id = manifest("BAD ID");
    bad_id.entry.command = "bin/run.exe".to_string();
    install_plugin(&tmp.join("bad-id"), &bad_id);
    // version 非法 semver → InvalidField
    fs::create_dir_all(tmp.join("bad-version")).expect("mkdir");
    let mut bad_ver = manifest("bad-version");
    bad_ver.version = "v1.2".to_string();
    install_plugin(&tmp.join("bad-version"), &bad_ver);
    // 合法插件照常发现（降级不中断）。
    fs::create_dir_all(tmp.join("ok")).expect("mkdir");
    install_plugin(&tmp.join("ok"), &manifest("ok"));

    let reg = registry(tmp.path(), &tmp.join("user"));
    let outcome = reg.discover();
    assert_eq!(outcome.plugins.len(), 1);
    assert_eq!(outcome.plugins[0].manifest.id, "ok");

    let find = |dir: &str| {
        outcome
            .invalid
            .iter()
            .find(|i| i.dir.ends_with(dir))
            .map(|i| i.reason.clone())
            .expect("invalid entry")
    };
    assert_eq!(find("no-manifest"), DiscoveryError::MissingManifest);
    assert_eq!(find("bad-id"), DiscoveryError::InvalidId);
    assert!(matches!(
        find("bad-version"),
        DiscoveryError::InvalidField(_)
    ));
}

#[test]
fn protocol_version_too_new_is_rejected() {
    let tmp = TempDir::new("proto-too-new");
    let dir = tmp.join("toonew");
    fs::create_dir_all(&dir).expect("mkdir");
    let mut m = manifest("toonew");
    m.min_protocol_version = 2;
    install_plugin(&dir, &m);

    let reg = registry(tmp.path(), &tmp.join("user"));
    let outcome = reg.discover();
    let reason = &outcome.invalid[0].reason;
    assert_eq!(
        reason.clone(),
        DiscoveryError::ProtocolVersionTooNew,
        "min_protocol_version > 1 must be rejected"
    );
}

#[test]
fn entry_resolved_relative_and_working_dir_defaults_to_plugin_dir() {
    let tmp = TempDir::new("entry-resolve");
    let dir = tmp.join("resolver");
    fs::create_dir_all(&dir).expect("mkdir");
    install_plugin(&dir, &manifest("resolver"));

    let reg = registry(tmp.path(), &tmp.join("user"));
    let outcome = reg.discover();
    let plugin = outcome.plugins.iter().find(|p| p.manifest.id == "resolver");
    let plugin = plugin.expect("resolver discovered");

    // command 含路径分隔符 → 相对插件目录解析为绝对路径。
    assert_eq!(plugin.resolved.program, canonical(&dir.join("bin/run.exe")));
    // working_dir 省略 → 默认 = plugin.json 所在目录。
    assert_eq!(plugin.resolved.working_dir, canonical(&dir));
    assert_eq!(plugin.resolved.args, vec!["--stdio".to_string()]);

    // working_dir 显式相对路径 → 相对插件目录解析。
    let mut m = manifest("resolver2");
    m.entry.working_dir = Some("sub".to_string());
    let d2 = tmp.join("resolver2");
    fs::create_dir_all(d2.join("sub")).expect("mkdir sub");
    install_plugin(&d2, &m);
    let outcome = reg.reload();
    let plugin = outcome
        .plugins
        .iter()
        .find(|p| p.manifest.id == "resolver2");
    let plugin = plugin.expect("resolver2 discovered");
    assert_eq!(plugin.resolved.working_dir, canonical(&d2.join("sub")));

    // working_dir 指向不存在目录 → EntryWorkingDirNotFound。
    let mut m = manifest("resolver3");
    m.entry.working_dir = Some("nope".to_string());
    let d3 = tmp.join("resolver3");
    fs::create_dir_all(&d3).expect("mkdir");
    install_plugin(&d3, &m);
    let outcome = reg.reload();
    let reason = outcome
        .invalid
        .iter()
        .find(|i| i.dir.ends_with("resolver3"))
        .map(|i| i.reason.clone());
    assert_eq!(reason, Some(DiscoveryError::EntryWorkingDirNotFound));

    // command 不存在 → EntryCommandNotFound。
    let mut m = manifest("resolver4");
    m.entry.command = "missing/run.exe".to_string();
    let d4 = tmp.join("resolver4");
    fs::create_dir_all(&d4).expect("mkdir");
    install_plugin(&d4, &m);
    let outcome = reg.reload();
    let reason = outcome
        .invalid
        .iter()
        .find(|i| i.dir.ends_with("resolver4"))
        .map(|i| i.reason.clone());
    assert_eq!(reason, Some(DiscoveryError::EntryCommandNotFound));
}

#[test]
fn reload_is_the_only_cache_invalidation_entry() {
    let tmp = TempDir::new("reload");
    let reg = registry(tmp.path(), &tmp.join("user"));
    assert!(reg.discover().plugins.is_empty());

    // 新插件出现后，discover() 仍返回缓存（空）。
    let dir = tmp.join("new-plugin");
    fs::create_dir_all(&dir).expect("mkdir");
    install_plugin(&dir, &manifest("new-plugin"));
    assert!(reg.discover().plugins.is_empty(), "cache must not refresh");

    // reload() 全量重扫 → 新插件出现，且事件已发布。
    let outcome = reg.reload();
    assert_eq!(outcome.plugins.len(), 1);
    assert_eq!(
        reg.get("new-plugin").map(|p| p.manifest.id),
        Some("new-plugin".into())
    );
    assert_eq!(reg.list().len(), 1);
}

#[test]
fn same_path_sources_are_deduplicated() {
    let tmp = TempDir::new("same-path-dedupe");
    let dir = tmp.join("shared");
    fs::create_dir_all(dir.join("only-one")).expect("mkdir");
    install_plugin(&dir.join("only-one"), &manifest("only-one"));

    // portable == install == user 全同路径 → 只扫一次。
    let reg = PluginRegistry::with_sources(dir.clone(), dir.clone(), dir);
    let outcome = reg.discover();
    assert_eq!(outcome.plugins.len(), 1, "same path must scan once");
    assert_eq!(outcome.plugins[0].source, PluginSource::Portable);
}

#[test]
fn disabled_plugins_are_excluded_from_discovery() {
    let tmp = TempDir::new("disabled");
    let portable = tmp.join("portable");
    fs::create_dir_all(portable.join("p1")).expect("mkdir p1");
    install_plugin(&portable.join("p1"), &manifest("p1"));
    let reg = registry(&portable, &tmp.join("user"));

    assert!(reg.list().iter().any(|p| p.manifest.id == "p1"));
    reg.set_disabled(&["p1".to_string()]);
    assert!(!reg.list().iter().any(|p| p.manifest.id == "p1"));
    assert!(reg.is_disabled("p1"));
    reg.set_disabled(&[]);
    assert!(reg.list().iter().any(|p| p.manifest.id == "p1"));
}

#[test]
fn same_source_duplicate_id_keeps_lexicographic_first() {
    let tmp = TempDir::new("same-source-dup");
    let src = tmp.join("src");
    fs::create_dir_all(src.join("dup-b")).expect("mkdir");
    fs::create_dir_all(src.join("dup-a")).expect("mkdir");
    install_plugin(&src.join("dup-b"), &manifest("dup"));
    install_plugin(&src.join("dup-a"), &manifest("dup"));

    let reg = PluginRegistry::with_sources(src.clone(), tmp.join("install"), tmp.join("user"));
    let outcome = reg.discover();
    assert_eq!(outcome.plugins.len(), 1);
    assert!(
        outcome.plugins[0].plugin_dir.ends_with("dup-a"),
        "lexicographically first dir wins"
    );
    assert_eq!(outcome.shadowed.len(), 1);
    assert_eq!(outcome.shadowed[0].winner_source, PluginSource::Portable);
}
