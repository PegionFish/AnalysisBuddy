//! `plugin check` CLI（docs-validator.md §1）：结构 + 行为两阶段校验、21 条冻结
//! 规则 ID、五档退出码；manifest 与协议帧只经 docs/spec 两份 JSON Schema 校验
//! （单源，§3.2）。
//!
//! 退出码（§1.3）：0 EXIT_PASS ｜ 1 EXIT_WARN ｜ 2 EXIT_ERROR ｜ 3 EXIT_USAGE ｜
//! 4 EXIT_INTERNAL。

mod behavior;
mod harness;
mod output;
mod rules;
mod structure;

use std::fs;
use std::path::{Path, PathBuf};

use jsonschema::{Draft, Validator as JsonSchemaValidator};
use serde_json::Value;

use crate::rules::{Finding, Level};

const EXIT_PASS: u32 = 0;
const EXIT_WARN: u32 = 1;
const EXIT_ERROR: u32 = 2;
const EXIT_USAGE: u32 = 3;
const EXIT_INTERNAL: u32 = 4;

const USAGE: &str = "\
plugin check <plugin_dir> [选项]

插件校验器：结构（MAN-01..MAN-09）+ 行为回放（BEH-01..BEH-12，--behavior）两阶段；
manifest 与协议帧只经 docs/spec 两份 JSON Schema 校验（单源）。

参数：
  <plugin_dir>            插件目录路径（即 plugin.json 所在子文件夹根，可以是独立 git 仓库根）

选项：
  --behavior              追加行为回放校验（拉起插件进程跑最小序列）；缺省只做结构校验
  --fixture <file>        行为回放用的日志文件；缺省使用内置 fixture（与 tests/fixtures 同源的小 CSV）
  --schema-dir <dir>      覆盖 Schema 查找路径（缺省：相对可执行文件定位 docs/spec/）
  --timeout-scale <f>     行为回放各超时按 f 倍缩放（慢机器/CI 用，缺省 1.0）
  --json                  以 JSON 输出结果（机器可读，供插件仓库 CI 消费）
  --host-version <ver>    模拟宿主版本（影响 MAN-05 判定，缺省为当前发布版 1）
  -h | --help             显示帮助
  -V | --version          显示版本

退出码：0 EXIT_PASS ｜ 1 EXIT_WARN ｜ 2 EXIT_ERROR ｜ 3 EXIT_USAGE ｜ 4 EXIT_INTERNAL
CI 门禁：exit code == 0 为通过线。";

struct Options {
    plugin_dir: PathBuf,
    behavior: bool,
    fixture: Option<PathBuf>,
    schema_dir: Option<PathBuf>,
    timeout_scale: f64,
    json: bool,
    host_version: u32,
}

enum Parsed {
    Run(Options),
    Help,
    Version,
}

fn parse_args<I: Iterator<Item = String>>(args: I) -> Result<Parsed, String> {
    let mut plugin_dir: Option<PathBuf> = None;
    let mut behavior = false;
    let mut fixture: Option<PathBuf> = None;
    let mut schema_dir: Option<PathBuf> = None;
    let mut timeout_scale = 1.0f64;
    let mut json = false;
    let mut host_version: u32 = 1;

    let mut it = args;
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Parsed::Help),
            "-V" | "--version" => return Ok(Parsed::Version),
            "--behavior" => behavior = true,
            "--json" => json = true,
            _ if arg == "--fixture"
                || arg == "--schema-dir"
                || arg == "--timeout-scale"
                || arg == "--host-version" =>
            {
                let value = it.next().ok_or_else(|| format!("`{arg}` 需要参数值"))?;
                match arg.as_str() {
                    "--fixture" => fixture = Some(PathBuf::from(value)),
                    "--schema-dir" => schema_dir = Some(PathBuf::from(value)),
                    "--timeout-scale" => {
                        timeout_scale = value
                            .parse::<f64>()
                            .map_err(|_| format!("--timeout-scale `{value}` 不是合法数值"))?;
                        if !timeout_scale.is_finite() || timeout_scale <= 0.0 {
                            return Err(format!("--timeout-scale 必须为正数（收到 {value}）"));
                        }
                    }
                    _ => {
                        host_version = parse_host_version(&value)
                            .map_err(|e| format!("--host-version：{e}"))?;
                    }
                }
            }
            _ if arg.starts_with("--fixture=") => {
                fixture = Some(PathBuf::from(arg["--fixture=".len()..].to_string()))
            }
            _ if arg.starts_with("--schema-dir=") => {
                schema_dir = Some(PathBuf::from(arg["--schema-dir=".len()..].to_string()))
            }
            _ if arg.starts_with("--timeout-scale=") => {
                let value = arg["--timeout-scale=".len()..].to_string();
                timeout_scale = value
                    .parse::<f64>()
                    .map_err(|_| format!("--timeout-scale `{value}` 不是合法数值"))?;
                if !timeout_scale.is_finite() || timeout_scale <= 0.0 {
                    return Err(format!("--timeout-scale 必须为正数（收到 {value}）"));
                }
            }
            _ if arg.starts_with("--host-version=") => {
                host_version = parse_host_version(&arg["--host-version=".len()..])
                    .map_err(|e| format!("--host-version：{e}"))?;
            }
            _ if arg.starts_with('-') => {
                return Err(format!("未知参数 `{arg}`"));
            }
            _ => {
                if plugin_dir.is_some() {
                    return Err(format!("多余的位置参数 `{arg}`（只需一个插件目录）"));
                }
                plugin_dir = Some(PathBuf::from(arg));
            }
        }
    }
    let Some(plugin_dir) = plugin_dir else {
        return Err("缺少 <plugin_dir> 位置参数".to_string());
    };
    Ok(Parsed::Run(Options {
        plugin_dir,
        behavior,
        fixture,
        schema_dir,
        timeout_scale,
        json,
        host_version,
    }))
}

/// 解析 `--host-version`：取前导数字段（如 "1"、"1.0-beta" → 1）。
fn parse_host_version(s: &str) -> Result<u32, String> {
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return Err(format!("`{s}` 无法解析为版本号（需以数字开头）"));
    }
    digits
        .parse::<u32>()
        .map_err(|e| format!("`{s}` 解析失败：{e}"))
}

fn real_main() -> u32 {
    let opts = match parse_args(std::env::args().skip(1)) {
        Ok(Parsed::Help) => {
            println!("{USAGE}");
            return EXIT_PASS;
        }
        Ok(Parsed::Version) => {
            println!("plugin-check {}", env!("CARGO_PKG_VERSION"));
            return EXIT_PASS;
        }
        Ok(Parsed::Run(o)) => o,
        Err(msg) => {
            eprintln!("plugin-check: {msg}\n\n{USAGE}");
            return EXIT_USAGE;
        }
    };

    if !opts.plugin_dir.is_dir() {
        eprintln!(
            "plugin-check: 插件目录不存在：`{}`（用法错误退出码 3；若目录存在但无 plugin.json，属 MAN-08 诊断，退出码 2）",
            opts.plugin_dir.display()
        );
        return EXIT_USAGE;
    }

    let (manifest_schema, rpc_schema) = match load_schemas(&opts) {
        Ok(v) => v,
        Err(msg) => {
            eprintln!("plugin-check: 内部故障（Schema 缺失或非法）：{msg}");
            return EXIT_INTERNAL;
        }
    };
    if let Some(p) = &opts.fixture {
        if !p.is_file() {
            eprintln!(
                "plugin-check: --fixture 文件不存在：`{}`（用法错误退出码 3）",
                p.display()
            );
            return EXIT_USAGE;
        }
    }

    // ---- Phase 1 结构校验 ----
    let mut findings = structure::run(&opts.plugin_dir, &manifest_schema, opts.host_version);
    let structure_errors = findings.iter().any(|f| f.level == Level::Error);

    // ---- Phase 2 行为回放（--behavior；结构 error 时跳过） ----
    let mut behavior_outcome: Option<behavior::BehaviorOutcome> = None;
    let mut internal_error: Option<String> = None;
    if opts.behavior {
        if structure_errors {
            // 结构阶段 error → 行为回放跳过（进程必拉不起来或结果无意义）
        } else {
            let fixture = match resolve_fixture(&opts) {
                Ok(f) => f,
                Err(msg) => {
                    eprintln!("plugin-check: 内部故障：{msg}");
                    return EXIT_INTERNAL;
                }
            };
            let manifest = match read_manifest(&opts.plugin_dir) {
                Ok(m) => m,
                Err(msg) => {
                    eprintln!("plugin-check: 内部故障：{msg}");
                    return EXIT_INTERNAL;
                }
            };
            let input = behavior::BehaviorInput {
                plugin_dir: opts.plugin_dir.clone(),
                manifest,
                fixture,
                scale: opts.timeout_scale,
                rpc_schema: &rpc_schema,
            };
            match behavior::run(&input) {
                Ok(mut out) => {
                    findings.append(&mut out.findings);
                    behavior_outcome = Some(out);
                }
                Err(msg) => internal_error = Some(msg),
            }
        }
    }
    if let Some(msg) = internal_error {
        eprintln!("plugin-check: 内部故障：{msg}");
        return EXIT_INTERNAL;
    }

    // ---- 汇总与输出 ----
    Finding::sort_by_rule(&mut findings);
    let exit_code = if findings.iter().any(|f| f.level == Level::Error) {
        EXIT_ERROR
    } else if findings.iter().any(|f| f.level == Level::Warning) {
        EXIT_WARN
    } else {
        EXIT_PASS
    };

    let phase1 = if structure_errors {
        output::PhaseStatus::Fail
    } else {
        output::PhaseStatus::Pass
    };
    let phase2 = match &behavior_outcome {
        Some(out) => {
            if out.aborted_at.is_some() {
                output::PhaseStatus::Skipped
            } else if findings
                .iter()
                .any(|f| f.rule_id.starts_with("BEH-") && f.level == Level::Error)
            {
                output::PhaseStatus::Fail
            } else {
                output::PhaseStatus::Pass
            }
        }
        None => output::PhaseStatus::Skipped,
    };

    let mut notes = Vec::new();
    if let Some(out) = &behavior_outcome {
        notes.extend(out.notes.iter().cloned());
        if let Some(at) = &out.aborted_at {
            notes.push(format!(
                "行为回放中止于 {at}（致命协议错误；其余 BEH 规则未评估）"
            ));
        }
    } else if opts.behavior && structure_errors {
        notes.push("行为回放跳过（结构阶段存在 error；进程必拉不起来或结果无意义）".to_string());
    } else if !opts.behavior {
        notes.push("追加 --behavior 可执行协议行为回放".to_string());
    }

    // passed_rules：适用的规则中无 Finding 者（MAN-09 为反向验收项，恒 pass）
    let mut passed: Vec<&'static str> = rules::RULE_IDS
        .iter()
        .filter(|id| !id.starts_with("BEH-"))
        .copied()
        .collect();
    let behavior_evaluated = behavior_outcome
        .as_ref()
        .is_some_and(|o| o.aborted_at.is_none());
    if behavior_evaluated {
        passed.extend(
            rules::RULE_IDS
                .iter()
                .filter(|id| id.starts_with("BEH-"))
                .copied(),
        );
    }
    let finding_ids: std::collections::HashSet<&str> = findings.iter().map(|f| f.rule_id).collect();
    passed.retain(|id| !finding_ids.contains(id));

    let report = output::Report {
        plugin_dir: &opts.plugin_dir,
        findings: &findings,
        passed_rules: passed,
        phase1,
        phase2,
        notes,
        stderr_dump: behavior_outcome
            .as_ref()
            .and_then(|o| o.stderr_dump.clone()),
        exit_code,
    };
    if opts.json {
        println!("{}", report.render_json());
    } else {
        print!("{}", report.render_human());
    }
    exit_code
}

/// 两份 Schema 加载（缺省相对可执行文件定位 `docs/spec/`，见 docs-validator.md §1.2）。
fn load_schemas(opts: &Options) -> Result<(JsonSchemaValidator, JsonSchemaValidator), String> {
    let dir = resolve_schema_dir(opts)?;
    let manifest_path = dir.join("plugin-manifest.schema.json");
    let rpc_path = dir.join("rpc-messages.schema.json");
    if !manifest_path.is_file() {
        return Err(format!(
            "缺少 `{}`（可用 --schema-dir 指定）",
            manifest_path.display()
        ));
    }
    if !rpc_path.is_file() {
        return Err(format!(
            "缺少 `{}`（可用 --schema-dir 指定）",
            rpc_path.display()
        ));
    }
    let build = |path: &Path, which: &str| -> Result<JsonSchemaValidator, String> {
        let text = fs::read_to_string(path).map_err(|e| format!("{which} 读取失败：{e}"))?;
        let value: Value =
            serde_json::from_str(&text).map_err(|e| format!("{which} 不是合法 JSON：{e}"))?;
        JsonSchemaValidator::options()
            .with_draft(Draft::Draft7)
            .build(&value)
            .map_err(|e| format!("{which} 不是合法 draft-07 Schema：{e}"))
    };
    let manifest = build(&manifest_path, "plugin-manifest.schema.json")?;
    let rpc = build(&rpc_path, "rpc-messages.schema.json")?;
    Ok((manifest, rpc))
}

fn resolve_schema_dir(opts: &Options) -> Result<PathBuf, String> {
    if let Some(d) = &opts.schema_dir {
        let canon = fs::canonicalize(d)
            .map_err(|e| format!("--schema-dir `{}` 不可访问：{e}", d.display()))?;
        if !canon.is_dir() {
            return Err(format!("--schema-dir `{}` 不是目录", d.display()));
        }
        return Ok(canon);
    }
    // 缺省：相对可执行文件向上定位 docs/spec/（最多 6 层），再回退当前目录
    if let Ok(exe) = std::env::current_exe() {
        let mut cur = exe.parent().map(Path::to_path_buf);
        for _ in 0..6 {
            if let Some(c) = cur {
                let candidate = c.join("docs/spec");
                if candidate.join("plugin-manifest.schema.json").is_file() {
                    return Ok(candidate);
                }
                cur = c.parent().map(Path::to_path_buf);
            }
        }
    }
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let candidate = cwd.join("docs/spec");
    if candidate.join("plugin-manifest.schema.json").is_file() {
        return Ok(candidate);
    }
    Err(
        "无法定位 docs/spec/（缺省：相对可执行文件路径与当前目录均未找到；可用 --schema-dir 指定）"
            .to_string(),
    )
}

/// fixture 解析：--fixture 优先；缺省用内置 `fixtures/small_with_header.csv`
/// （与 tests/fixtures 同源同格式，docs-validator.md §3.3）。
fn resolve_fixture(opts: &Options) -> Result<PathBuf, String> {
    if let Some(p) = &opts.fixture {
        return fs::canonicalize(p).map_err(|e| format!("--fixture 解析失败：{e}"));
    }
    let candidates = [
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/small_with_header.csv"),
        std::env::current_exe()
            .ok()
            .and_then(|exe| exe.parent().map(|p| p.to_path_buf()))
            .unwrap_or_default()
            .join("fixtures/small_with_header.csv"),
        PathBuf::from("fixtures/small_with_header.csv"),
    ];
    candidates
        .into_iter()
        .find(|p| p.is_file())
        .ok_or_else(|| "无法定位内置 fixture fixtures/small_with_header.csv".to_string())
}

fn read_manifest(plugin_dir: &Path) -> Result<ab_protocol::manifest::Manifest, String> {
    let path = plugin_dir.join("plugin.json");
    let text = fs::read_to_string(&path).map_err(|e| format!("读取 {path:?} 失败：{e}"))?;
    serde_json::from_str(&text).map_err(|e| format!("解析 {path:?} 失败：{e}"))
}

fn main() {
    std::process::exit(real_main() as i32);
}
