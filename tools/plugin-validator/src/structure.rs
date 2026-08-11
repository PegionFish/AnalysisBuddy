//! Phase 1 结构校验：MAN-01 ~ MAN-13（docs-validator.md §2.1）。
//!
//! 流程：目录模型检查（MAN-08/MAN-09）→ JSON Schema 校验（MAN-01）→ 语义检查
//! （MAN-02/03/04/05/06/07/10/11/12/13）。结构阶段出现 error 时，`--behavior` 直接跳过。
//!
//! 单源纪律（docs-validator.md §3.2）：manifest 只经
//! `docs/spec/plugin-manifest.schema.json` 校验——本模块不内嵌任何第二套结构断言
//! （id 格式、必填字段、类型等全部由 Schema 判定，见 `MAN-01` 检测方式）。

use std::fs;
use std::path::{Path, PathBuf};

use jsonschema::Validator as JsonSchemaValidator;
use serde_json::Value;

use crate::rules::Finding;

/// Phase 1 结构校验入口。返回全部 MAN 规则 Finding（error / warning）。
pub fn run(
    plugin_dir: &Path,
    manifest_schema: &JsonSchemaValidator,
    host_version: u32,
) -> Vec<Finding> {
    let mut findings = Vec::new();

    // ---- ① 目录模型检查（MAN-08） ----
    let root_manifests = find_plugin_json_at(plugin_dir);
    let nested_manifests = find_nested_plugin_json(plugin_dir);

    if root_manifests.is_empty() {
        if let Some(nested) = nested_manifests.first() {
            findings.push(Finding::error(
                "MAN-08",
                format!(
                    "plugin.json 不在插件目录根部：出现在 `{}`；发现规则要求清单位于插件文件夹根部（protocol-v1.md §7.1 第 2 条）",
                    display_path(nested)
                ),
                "plugin.json",
            ));
        } else {
            findings.push(Finding::error(
                "MAN-08",
                "目录中不存在 plugin.json；发现规则要求插件文件夹根部有唯一 plugin.json（protocol-v1.md §7.1）",
                "plugin.json",
            ));
        }
        return findings;
    }
    if root_manifests.len() > 1 {
        for extra in root_manifests.iter().skip(1) {
            findings.push(Finding::error(
                "MAN-08",
                "插件目录根部存在多个 plugin.json（发现规则只认根级唯一清单）",
                display_path(extra),
            ));
        }
    }
    // 根清单存在时，深层嵌套的额外清单同样报 MAN-08（"有多个 plugin.json" 情形）
    for nested in &nested_manifests {
        findings.push(Finding::error(
            "MAN-08",
            format!(
                "plugin.json 出现在子目录 `{}`（发现规则只认根级唯一清单；请迁移到插件文件夹根部，protocol-v1.md §7.1 第 2 条）",
                display_path(nested)
            ),
            display_path(nested),
        ));
    }

    let root_manifest_path = &root_manifests[0];
    let text = match fs::read_to_string(root_manifest_path) {
        Ok(t) => t,
        Err(e) => {
            findings.push(Finding::error(
                "MAN-01",
                format!("plugin.json 无法读取：{e}"),
                display_path(root_manifest_path),
            ));
            return findings;
        }
    };
    let manifest: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            findings.push(Finding::error(
                "MAN-01",
                format!("plugin.json 不是合法 JSON：{e}"),
                display_path(root_manifest_path),
            ));
            return findings;
        }
    };

    // ---- ② JSON Schema 校验（MAN-01；required + 类型/pattern 断言逐字段报告） ----
    record_schema_findings(&mut findings, manifest_schema, &manifest);

    // ---- ③ 语义检查 ----
    semantic_checks(
        &mut findings,
        plugin_dir,
        &manifest,
        root_manifest_path,
        nested_manifests,
        host_version,
    );

    findings
}

/// ② manifest 对 plugin-manifest.schema.json 的校验结果 → MAN-01 Finding。
/// 定位与 Schema 校验的 instancePath 对齐（`plugin.json#/…`）。
fn record_schema_findings(
    findings: &mut Vec<Finding>,
    schema: &JsonSchemaValidator,
    manifest: &Value,
) {
    let mut count = 0usize;
    for err in schema.iter_errors(manifest) {
        count += 1;
        if count <= 20 {
            findings.push(Finding::error(
                "MAN-01",
                err.to_string(),
                format!("plugin.json{}", err.instance_path),
            ));
        }
    }
    if count > 20 {
        findings.push(Finding::error(
            "MAN-01",
            format!("…（共 {count} 处 Schema 违规，仅列出前 20 条）"),
            "plugin.json",
        ));
    }
}

/// ③ 语义检查：MAN-02/03/04/05/06/07/10/11/12/13。MAN-09 为反向验收项（无 Finding）。
#[allow(clippy::too_many_arguments)]
fn semantic_checks(
    findings: &mut Vec<Finding>,
    plugin_dir: &Path,
    manifest: &Value,
    root_manifest_path: &Path,
    nested_manifests: Vec<PathBuf>,
    host_version: u32,
) {
    // ---- MAN-02：id 与目录名冲突 / 目录树内重复 ----
    let dir_name = plugin_dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string();
    if let Some(id) = manifest.get("id").and_then(Value::as_str) {
        if id != dir_name {
            findings.push(Finding::error(
                "MAN-02",
                format!(
                    "manifest id `{id}` 与插件目录名 `{dir_name}` 不一致（发现模型以目录名为物理锚点，protocol-v1.md §7.1）"
                ),
                "plugin.json#/id",
            ));
        }
        for (path, other) in collect_tree_manifests(plugin_dir) {
            if path == *root_manifest_path {
                continue; // 根清单自身不参与重复比对
            }
            if other.get("id").and_then(Value::as_str) == Some(id) {
                findings.push(Finding::error(
                    "MAN-02",
                    format!("目录树内发现重复 id `{id}`（`{}`）", display_path(&path)),
                    "plugin.json#/id",
                ));
            }
        }
    }

    // ---- MAN-03 / MAN-04：entry 存在性与路径形态 ----
    let entry = manifest.get("entry");
    let command = entry.and_then(|e| e.get("command")).and_then(Value::as_str);
    let working_dir = entry
        .and_then(|e| e.get("working_dir"))
        .and_then(Value::as_str);

    // MAN-04（warning）先行，路径形态不影响存在性判断
    if let Some(cmd) = command {
        if is_absolute_path(cmd) {
            findings.push(Finding::warn(
                "MAN-04",
                format!("entry.command 使用绝对路径 `{cmd}`；应改为相对 plugin.json 所在目录的路径（破坏「仓库拖入即用」的可移植性）"),
                "plugin.json#/entry/command",
            ));
        }
    }
    if let Some(wd) = working_dir {
        if is_absolute_path(wd) {
            findings.push(Finding::warn(
                "MAN-04",
                format!(
                    "entry.working_dir 使用绝对路径 `{wd}`；应改为相对 plugin.json 所在目录的路径"
                ),
                "plugin.json#/entry/working_dir",
            ));
        }
    }

    // MAN-03：command 必须是已存在文件；解释器型入口查 PATH，查不到降级 warning
    if let Some(cmd) = command {
        let absolute = is_absolute_path(cmd);
        let resolved = if absolute {
            PathBuf::from(cmd)
        } else {
            plugin_dir.join(cmd)
        };
        let exists = resolved.is_file()
            || (!absolute && PathBuf::from(format!("{}.exe", resolved.display())).is_file());
        if !exists {
            if is_interpreter_command(cmd) {
                let in_path = find_in_path(cmd).is_some();
                if !in_path {
                    findings.push(Finding::warn(
                        "MAN-03",
                        format!(
                            "解释器型入口 `{cmd}` 未在 PATH/py launcher 找到（降级为 warning；protocol-v1.md §7.3 允许解释器按系统约定查找）"
                        ),
                        "plugin.json#/entry/command",
                    ));
                }
            } else {
                findings.push(Finding::error(
                    "MAN-03",
                    format!(
                        "entry.command `{cmd}` 指向的文件不存在（相对 plugin.json 目录解析为 `{}`）",
                        display_path(&resolved)
                    ),
                    "plugin.json#/entry/command",
                ));
            }
        }
    }
    if let Some(wd) = working_dir {
        let absolute = is_absolute_path(wd);
        let resolved = if absolute {
            PathBuf::from(wd)
        } else {
            plugin_dir.join(wd)
        };
        if !resolved.is_dir() {
            findings.push(Finding::error(
                "MAN-03",
                format!(
                    "entry.working_dir `{wd}` 解析后不是已存在目录（`{}`）",
                    display_path(&resolved)
                ),
                "plugin.json#/entry/working_dir",
            ));
        }
    }

    // ---- MAN-05：min_protocol_version 超限 ----
    if let Some(mpv) = manifest.get("min_protocol_version").and_then(Value::as_u64) {
        if mpv > u64::from(host_version) {
            findings.push(Finding::error(
                "MAN-05",
                format!(
                    "min_protocol_version = {mpv} 高于宿主支持版本 {host_version}；插件不会被加载，宿主建议升级（protocol-v1.md §7.2 加载拒绝语义）。可用 --host-version 指定宿主版本后复验"
                ),
                "plugin.json#/min_protocol_version",
            ));
        }
    }

    // ---- MAN-06：match 双空 ----
    let m = manifest.get("match");
    let exts_empty = m
        .and_then(|m| m.get("extensions"))
        .and_then(Value::as_array)
        .is_none_or(|a| a.is_empty());
    let fps_empty = m
        .and_then(|m| m.get("header_fingerprints"))
        .and_then(Value::as_array)
        .is_none_or(|a| a.is_empty());
    if exts_empty && fps_empty {
        findings.push(Finding::warn(
            "MAN-06",
            "match.extensions 与 header_fingerprints 同时为空；该插件永远无法被自动发现，只能用户手选（protocol-v1.md §7.2）",
            "plugin.json#/match",
        ));
    }

    // ---- MAN-07：version 非 semver（宽松解析，允许 build metadata） ----
    // 说明：plugin-manifest.schema.json 的 version pattern 为严格 semver，
    // 非 semver 版本会被 MAN-01（Schema 判据）先拦下；MAN-07 为「宽松解析」的
    // 独立检查层——当 Schema 判据通过而宽松解析仍失败时（如未来 Schema pattern
    // 放宽），MAN-07 保持生效，防双源漂移。
    if let Some(version) = manifest.get("version").and_then(Value::as_str) {
        if !is_lax_semver(version) {
            findings.push(Finding::warn(
                "MAN-07",
                format!("version `{version}` 不是语义化版本号（宽松解析，允许 build metadata）"),
                "plugin.json#/version",
            ));
        }
    }

    // ---- MAN-10：author/repository 格式（可选字段提供时的形态约束） ----
    // 类型判据由 Schema 承担（MAN-01）；此处只做 Schema 无法表达的格式语义：
    // author 非空、repository 为 https URL（TLS 强制，模块管理器设计 §3.1/§5.2）。
    if let Some(author) = manifest.get("author").and_then(Value::as_str) {
        if author.trim().is_empty() {
            findings.push(Finding::error(
                "MAN-10",
                "author 为空字符串；可选字段一旦提供须为非空字符串（模块管理器设计 §3.1）",
                "plugin.json#/author",
            ));
        }
    }
    if let Some(repo) = manifest.get("repository").and_then(Value::as_str) {
        if !is_https_url(repo) {
            findings.push(Finding::error(
                "MAN-10",
                format!(
                    "repository `{repo}` 不是合法 https URL（模块管理器设计 §3.1：TLS 强制，拒绝明文 http）"
                ),
                "plugin.json#/repository",
            ));
        }
    }

    // ---- MAN-11：tools 约束语法（每条必须解析为 `{tool} {VersionReq}`） ----
    // 宿主自检（§4.2 安装管线第④步）依赖该语法；语法非法 → 适配无法评估 → 模块 invalid。
    if let Some(tools) = manifest.get("tools").and_then(Value::as_array) {
        for (i, t) in tools.iter().enumerate() {
            let Some(entry) = t.as_str() else {
                continue; // 非字符串由 MAN-01 类型判据拦截
            };
            let mut parts = entry.split_whitespace();
            let Some(tool) = parts.next() else {
                findings.push(Finding::error(
                    "MAN-11",
                    format!(
                        "tools 第 {} 项为空字符串；约束语法应为 `{{tool}} {{VersionReq}}`（如 `AnalysisBuddy >= 0.2.0`，模块管理器设计 §3.1）",
                        i + 1
                    ),
                    format!("plugin.json#/tools/{i}"),
                ));
                continue;
            };
            let req: String = parts.collect::<Vec<_>>().join(" ");
            if req.is_empty() {
                findings.push(Finding::error(
                    "MAN-11",
                    format!(
                        "tools 第 {} 项工具 `{tool}` 缺少 VersionReq；约束语法应为 `{{tool}} {{VersionReq}}`（如 `AnalysisBuddy >= 0.2.0`，模块管理器设计 §3.1）",
                        i + 1
                    ),
                    format!("plugin.json#/tools/{i}"),
                ));
            } else if semver::VersionReq::parse(&req).is_err() {
                findings.push(Finding::error(
                    "MAN-11",
                    format!(
                        "tools 第 {} 项 `{entry}` 的 VersionReq `{req}` 不是合法 semver 版本约束（如 `>= 0.2.0`、`^1.0`，模块管理器设计 §3.1）",
                        i + 1
                    ),
                    format!("plugin.json#/tools/{i}"),
                ));
            }
        }
    }

    // ---- MAN-12：changelog 结构（version semver / date YYYY-MM-DD / notes 数组） ----
    // 类型判据由 Schema 承担（MAN-01，changelog 非数组、条目非对象被拦截）；
    // 此处逐条做 Schema 未表达的格式语义，层间关系同 MAN-07（宽松层漂移守护）。
    if let Some(changelog) = manifest.get("changelog") {
        if let Some(entries) = changelog.as_array() {
            for (i, entry) in entries.iter().enumerate() {
                let Some(obj) = entry.as_object() else {
                    continue; // 非对象条目由 MAN-01 类型判据拦截
                };
                match obj.get("version").and_then(Value::as_str) {
                    None => findings.push(Finding::error(
                        "MAN-12",
                        format!("changelog 第 {} 项缺 version；每条须含 version/date/notes 三字段（模块管理器设计 §3.1）", i + 1),
                        format!("plugin.json#/changelog/{i}"),
                    )),
                    Some(v) if !is_lax_semver(v) => findings.push(Finding::error(
                        "MAN-12",
                        format!(
                            "changelog 第 {} 项 version `{v}` 不是语义化版本号（宽松解析，允许 build metadata）",
                            i + 1
                        ),
                        format!("plugin.json#/changelog/{i}/version"),
                    )),
                    _ => {}
                }
                match obj.get("date").and_then(Value::as_str) {
                    None => findings.push(Finding::error(
                        "MAN-12",
                        format!("changelog 第 {} 项缺 date；每条须含 version/date/notes 三字段（模块管理器设计 §3.1）", i + 1),
                        format!("plugin.json#/changelog/{i}"),
                    )),
                    Some(d) if !is_yyyy_mm_dd(d) => findings.push(Finding::error(
                        "MAN-12",
                        format!(
                            "changelog 第 {} 项 date `{d}` 不是 YYYY-MM-DD 格式（且月 01..12、日 01..31）",
                            i + 1
                        ),
                        format!("plugin.json#/changelog/{i}/date"),
                    )),
                    _ => {}
                }
                let notes_ok = obj
                    .get("notes")
                    .is_some_and(|n| n.as_array().is_some_and(|a| a.iter().all(Value::is_string)));
                if !notes_ok {
                    findings.push(Finding::error(
                        "MAN-12",
                        format!(
                            "changelog 第 {} 项 notes 必须是字符串数组；每条须含 version/date/notes 三字段（模块管理器设计 §3.1）",
                            i + 1
                        ),
                        format!("plugin.json#/changelog/{i}/notes"),
                    ));
                }
            }
        }
    }

    // ---- MAN-13：changelog 一致性（非空时版本严格降序 + 当前 version 在列） ----
    // 排序语义：严格降序（相邻两版本必须前 > 后）；无法严格解析的版本已由
    // MAN-12 报告，此处跳过避免级联噪音。
    if let Some(changelog) = manifest.get("changelog").and_then(Value::as_array) {
        if !changelog.is_empty() {
            let parsed: Vec<Option<semver::Version>> = changelog
                .iter()
                .map(|e| {
                    e.get("version")
                        .and_then(Value::as_str)
                        .and_then(|v| semver::Version::parse(v).ok())
                })
                .collect();
            for pair in parsed.windows(2) {
                if let (Some(a), Some(b)) = (&pair[0], &pair[1]) {
                    if a <= b {
                        findings.push(Finding::error(
                            "MAN-13",
                            format!(
                                "changelog 版本必须严格降序：`{a}` 之后出现 `{b}`（模块管理器设计 §6.2 版本历史渲染以本字段为准）"
                            ),
                            "plugin.json#/changelog",
                        ));
                    }
                }
            }
            if let Some(cur) = manifest.get("version").and_then(Value::as_str) {
                let in_list = if let Ok(v) = semver::Version::parse(cur) {
                    parsed.iter().any(|ov| ov.as_ref() == Some(&v))
                } else {
                    changelog
                        .iter()
                        .any(|e| e.get("version").and_then(Value::as_str) == Some(cur))
                };
                if !in_list {
                    findings.push(Finding::error(
                        "MAN-13",
                        format!(
                            "changelog 不含当前版本 `{cur}`；版本历史必须包含 manifest 的 version（模块管理器设计 §3.1）"
                        ),
                        "plugin.json#/changelog",
                    ));
                }
            }
        }
    }

    let _ = nested_manifests; // 重复 id 扫描已并入 MAN-02
}

// MAN-09 反向验收说明：无关文件容忍不产生任何 Finding——目录内存在 `.git/`、
// 源码、构建中间产物不得触发 error/warning（protocol-v1.md §7.1 第 3 条）。
// 其验收由 `tests/rules_manifest.rs::man_09_ignores_unrelated_files` 断言零告警。

// ---------------------------------------------------------------------------
// 纯函数工具（可单测）
// ---------------------------------------------------------------------------

/// 绝对路径判定：盘符前缀（`C:\` / `C:/`）、UNC（`\\server`）、根相对（`\`、`/`）。
pub fn is_absolute_path(s: &str) -> bool {
    if s.starts_with('\\') || s.starts_with('/') {
        return true;
    }
    let bytes = s.as_bytes();
    bytes.len() >= 3
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
        && bytes[0].is_ascii_alphabetic()
}

/// 宽松 semver：`主.次.修` + 可选 `-pre` / `+build`（允许 build metadata）。
/// 与 Schema 严格 pattern 的关系见 `MAN-07` 注释。
pub fn is_lax_semver(s: &str) -> bool {
    let mut parts = s.splitn(3, '.');
    let (Some(major), Some(minor), Some(rest)) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    let (patch, suffix) = rest
        .split_once(['-', '+'])
        .map_or((rest, None), |(p, s)| (p, Some(s)));
    if patch.contains('.') {
        return false;
    }
    if suffix.is_some_and(|s| s.is_empty()) {
        return false;
    }
    !major.is_empty()
        && major.chars().all(|c| c.is_ascii_digit())
        && !minor.is_empty()
        && minor.chars().all(|c| c.is_ascii_digit())
        && !patch.is_empty()
        && patch.chars().all(|c| c.is_ascii_digit())
}

/// https URL 判定（MAN-10）：`https://` 前缀 + 其余部分非空且不含空白。
/// TLS 强制（模块管理器设计 §5.2 拒绝明文 http），故 http:// 一律非法。
pub fn is_https_url(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("https://") else {
        return false;
    };
    !rest.is_empty() && !rest.chars().any(|c| c.is_whitespace())
}

/// `YYYY-MM-DD` 格式判定（MAN-12）：10 字符形态 + 月 01..12、日 01..31。
pub fn is_yyyy_mm_dd(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return false;
    }
    let digit = |i: usize| (b[i] as char).is_ascii_digit();
    if !(0..10).all(|i| i == 4 || i == 7 || digit(i)) {
        return false;
    }
    let month = (b[5] - b'0') * 10 + (b[6] - b'0');
    let day = (b[8] - b'0') * 10 + (b[9] - b'0');
    (1..=12).contains(&month) && (1..=31).contains(&day)
}

/// 解释器型入口判定（protocol-v1.md §7.3 唯一例外）：`python`/`py`/`python3`
/// （可带 `.exe` 后缀），按系统约定从 PATH/py launcher 查找。
pub fn is_interpreter_command(cmd: &str) -> bool {
    let base = cmd.strip_suffix(".exe").unwrap_or(cmd).to_ascii_lowercase();
    matches!(base.as_str(), "python" | "py" | "python3")
}

/// 在 PATH（+ PATHEXT）中查找可执行文件；找到返回其路径。
pub fn find_in_path(name: &str) -> Option<PathBuf> {
    if is_absolute_path(name) {
        return Path::new(name).is_file().then(|| PathBuf::from(name));
    }
    let exts: Vec<String> = std::env::var("PATHEXT")
        .unwrap_or_else(|_| ".EXE;.COM;.BAT;.CMD".to_string())
        .split(';')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_ascii_lowercase())
        .collect();
    for dir in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        let bare = dir.join(name);
        if bare.is_file() {
            return Some(bare);
        }
        if !bare.extension().is_some() {
            for ext in &exts {
                let candidate = dir.join(format!("{name}{ext}"));
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

fn display_path(p: &Path) -> String {
    p.display().to_string()
}

/// 插件目录根部所有 `plugin.json`（正常应为 0 或 1 个）。
fn find_plugin_json_at(plugin_dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(plugin_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_type().is_ok_and(|t| t.is_file())
                && entry.file_name().to_string_lossy() == "plugin.json"
            {
                out.push(path);
            }
        }
    }
    out
}

/// 树内（不含根部）名为 `plugin.json` 的文件。
fn find_nested_plugin_json(plugin_dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(plugin_dir, &mut out, true);
    out
}

/// 树内全部 `plugin.json` 及其解析结果（重复 id 扫描用）。
fn collect_tree_manifests(plugin_dir: &Path) -> Vec<(PathBuf, Value)> {
    let mut paths = Vec::new();
    walk(plugin_dir, &mut paths, false);
    let mut out = Vec::new();
    for p in paths {
        if let Ok(text) = fs::read_to_string(&p) {
            if let Ok(v) = serde_json::from_str::<Value>(&text) {
                out.push((p, v));
            }
        }
    }
    out
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>, skip_root: bool) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let ft = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if ft.is_dir() {
            walk(&path, out, false);
        } else if ft.is_file()
            && entry.file_name().to_string_lossy() == "plugin.json"
            && !(skip_root && path.parent() == Some(dir))
        {
            out.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_path_detection() {
        assert!(is_absolute_path(r"C:\tools\run.exe"));
        assert!(is_absolute_path("C:/tools/run.exe"));
        assert!(is_absolute_path(r"\\server\share\run.exe"));
        assert!(is_absolute_path(r"\root-relative"));
        assert!(is_absolute_path("/posix"));
        assert!(!is_absolute_path("python"));
        assert!(!is_absolute_path("target/release/builtin-csv.exe"));
        assert!(!is_absolute_path(r".\plugin.py"));
        assert!(!is_absolute_path("run.exe"));
    }

    #[test]
    fn lax_semver() {
        assert!(is_lax_semver("0.1.0"));
        assert!(is_lax_semver("1.2.3+build.5"));
        assert!(is_lax_semver("0.1.0-beta.1"));
        assert!(!is_lax_semver("1.0"));
        assert!(!is_lax_semver("v1.0.0"));
        assert!(!is_lax_semver("1.0.0.0"));
        assert!(!is_lax_semver(""));
        assert!(!is_lax_semver("1.2.3-"));
        assert!(is_lax_semver("10.20.30"));
    }

    #[test]
    fn interpreter_commands() {
        assert!(is_interpreter_command("python"));
        assert!(is_interpreter_command("py"));
        assert!(is_interpreter_command("python3"));
        assert!(is_interpreter_command("python.exe"));
        assert!(!is_interpreter_command("target/release/builtin-csv.exe"));
        assert!(!is_interpreter_command("node"));
    }

    #[test]
    fn https_url() {
        assert!(is_https_url("https://github.com/owner/repo"));
        assert!(is_https_url("https://github.com"));
        assert!(!is_https_url("http://github.com/owner/repo"));
        assert!(!is_https_url("https://"));
        assert!(!is_https_url("https:// github.com/repo"));
        assert!(!is_https_url("github.com/owner/repo"));
        assert!(!is_https_url(""));
        assert!(!is_https_url("HTTPS://github.com/owner/repo"));
    }

    #[test]
    fn yyyy_mm_dd() {
        assert!(is_yyyy_mm_dd("2026-08-01"));
        assert!(is_yyyy_mm_dd("2026-12-31"));
        assert!(is_yyyy_mm_dd("2026-01-01"));
        assert!(!is_yyyy_mm_dd("2026-13-01"));
        assert!(!is_yyyy_mm_dd("2026-00-10"));
        assert!(!is_yyyy_mm_dd("2026-08-00"));
        assert!(!is_yyyy_mm_dd("2026-08-32"));
        assert!(!is_yyyy_mm_dd("26-08-01"));
        assert!(!is_yyyy_mm_dd("2026/08/01"));
        assert!(!is_yyyy_mm_dd("2026-8-1"));
        assert!(!is_yyyy_mm_dd(""));
    }
}
