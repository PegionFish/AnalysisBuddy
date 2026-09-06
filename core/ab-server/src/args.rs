//! CLI 解析（clap-free 手写旗标，与 ab-app 手工参数风格一致）与
//! [`ab_engine::paths::EnginePaths`] 平台默认公式。
//!
//! 默认路径：非 Windows 走 [`EnginePaths::linux_default()`]（XDG）；Windows
//! 走桌面公式等价（exe 同目录 `plugins` 便携源 + `%APPDATA%\AnalysisBuddy`
//! 数据根），保证 dev 环境与桌面壳看到的目录一致。`--user-data-dir` 一次
//! 设齐 user 插件/presets/sessions 三个子目录，个别旗标可再覆盖。

use std::path::{Path, PathBuf};

use ab_engine::paths::EnginePaths;

/// 服务端全部旗标（[`ServerArgs::default`] 无副作用；环境变量读取仅发生在
/// [`resolve_paths`] 内）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerArgs {
    /// 监听地址（`ip:port`）。
    pub addr: String,
    /// Bearer 令牌（缺省不启用认证）。
    pub token: Option<String>,
    /// 并发导入上限（Semaphore 容量；≥1）。
    pub max_concurrent_imports: usize,
    pub plugins_portable: Option<PathBuf>,
    pub plugins_install: Option<PathBuf>,
    pub plugins_user: Option<PathBuf>,
    pub presets_dir: Option<PathBuf>,
    pub sessions_dir: Option<PathBuf>,
    /// 快捷：一次设置 plugins-user / presets / sessions 三个子目录。
    pub user_data_dir: Option<PathBuf>,
}

impl Default for ServerArgs {
    fn default() -> Self {
        Self {
            addr: "127.0.0.1:8600".to_string(),
            token: None,
            max_concurrent_imports: 2,
            plugins_portable: None,
            plugins_install: None,
            plugins_user: None,
            presets_dir: None,
            sessions_dir: None,
            user_data_dir: None,
        }
    }
}

/// `--help` 文本（main.rs 直接打印）。
pub const USAGE: &str = "\
ab-server - AnalysisBuddy headless HTTP+SSE service (protocol v1)

USAGE:
    ab-server [FLAGS]

FLAGS:
    --addr <ip:port>               Listen address (default 127.0.0.1:8600)
    --token <token>                Require `Authorization: Bearer <token>` on
                                   every endpoint except GET /api/v1/health
    --max-concurrent-imports <n>   Import concurrency gate (default 2, min 1)
    --plugins-portable <dir>       Portable plugin source dir (module state
                                   file lives here too)
    --plugins-install <dir>        Install-source plugin dir (defaults to the
                                   portable dir)
    --plugins-user <dir>           User-data plugin dir
    --presets-dir <dir>            User presets dir
    --sessions-dir <dir>           Session dir (save/load path root)
    --user-data-dir <dir>          Shorthand: set plugins-user / presets /
                                   sessions to <dir>/{plugins,presets,sessions}
    -h, --help                     Print this help
";

/// 解析参数（旗标支持 `--flag value` 与 `--flag=value` 两种形态）。
/// 未知旗标 / 缺值 / `--max-concurrent-imports` 非 usize 或 0 → Err。
pub fn parse_args(argv: &[String]) -> Result<ServerArgs, String> {
    let mut args = ServerArgs::default();
    let mut i = 0;
    while i < argv.len() {
        let raw = argv[i].as_str();
        let (name, inline) = match raw.split_once('=') {
            Some((n, v)) => (n, Some(v.to_string())),
            None => (raw, None),
        };
        match name {
            "--addr" => args.addr = take_value(argv, &mut i, inline, "--addr")?,
            "--token" => args.token = Some(take_value(argv, &mut i, inline, "--token")?),
            "--max-concurrent-imports" => {
                let value = take_value(argv, &mut i, inline, "--max-concurrent-imports")?;
                let parsed: usize = value
                    .parse()
                    .map_err(|_| format!("--max-concurrent-imports: not a number: {value}"))?;
                if parsed == 0 {
                    return Err("--max-concurrent-imports must be >= 1".to_string());
                }
                args.max_concurrent_imports = parsed;
            }
            "--plugins-portable" => {
                args.plugins_portable = Some(path_value(argv, &mut i, inline, "--plugins-portable")?);
            }
            "--plugins-install" => {
                args.plugins_install = Some(path_value(argv, &mut i, inline, "--plugins-install")?);
            }
            "--plugins-user" => {
                args.plugins_user = Some(path_value(argv, &mut i, inline, "--plugins-user")?);
            }
            "--presets-dir" => {
                args.presets_dir = Some(path_value(argv, &mut i, inline, "--presets-dir")?);
            }
            "--sessions-dir" => {
                args.sessions_dir = Some(path_value(argv, &mut i, inline, "--sessions-dir")?);
            }
            "--user-data-dir" => {
                args.user_data_dir = Some(path_value(argv, &mut i, inline, "--user-data-dir")?);
            }
            other => return Err(format!("unknown flag: {other}")),
        }
        i += 1;
    }
    Ok(args)
}

/// 取旗标值：inline（`--flag=value`）优先，否则取下一 argv。
fn take_value(
    argv: &[String],
    i: &mut usize,
    inline: Option<String>,
    flag: &str,
) -> Result<String, String> {
    if let Some(value) = inline {
        return Ok(value);
    }
    match argv.get(*i + 1) {
        Some(value) => {
            *i += 1;
            Ok(value.clone())
        }
        None => Err(format!("{flag}: missing value")),
    }
}

fn path_value(
    argv: &[String],
    i: &mut usize,
    inline: Option<String>,
    flag: &str,
) -> Result<PathBuf, String> {
    Ok(PathBuf::from(take_value(argv, i, inline, flag)?))
}

/// 由旗标覆盖平台默认路径（user-data-dir 先设三子目录，个别旗标再覆盖）。
pub fn resolve_paths(args: &ServerArgs) -> EnginePaths {
    let mut paths = default_paths();
    if let Some(dir) = &args.user_data_dir {
        paths.plugins_user = dir.join("plugins");
        paths.presets_dir = dir.join("presets");
        paths.sessions_dir = dir.join("sessions");
    }
    if let Some(dir) = &args.plugins_portable {
        paths.plugins_portable = dir.clone();
    }
    if let Some(dir) = &args.plugins_install {
        paths.plugins_install = dir.clone();
    }
    if let Some(dir) = &args.plugins_user {
        paths.plugins_user = dir.clone();
    }
    if let Some(dir) = &args.presets_dir {
        paths.presets_dir = dir.clone();
    }
    if let Some(dir) = &args.sessions_dir {
        paths.sessions_dir = dir.clone();
    }
    paths
}

/// 平台默认（无任何旗标时）：非 Windows XDG；Windows 桌面公式等价。
fn default_paths() -> EnginePaths {
    if cfg!(windows) {
        windows_default()
    } else {
        EnginePaths::linux_default()
    }
}

/// Windows 桌面公式等价（paths.rs 头注同款）：便携源 = exe 同目录
/// `plugins`（install 源同路径，ZIP 布局下与 Portable 重合）；数据根 =
/// `%APPDATA%\AnalysisBuddy`（缺失回落 `%USERPROFILE%\AppData\Roaming`，
/// 再回落空路径——调用方注入显式旗标覆盖）。
fn windows_default() -> EnginePaths {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .unwrap_or_default();
    let roaming = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .map(|home| PathBuf::from(home).join("AppData").join("Roaming"))
        })
        .unwrap_or_default();
    let base = roaming.join("AnalysisBuddy");
    EnginePaths {
        plugins_portable: exe_dir.join("plugins"),
        plugins_install: exe_dir.join("plugins"),
        plugins_user: base.join("plugins"),
        presets_dir: base.join("presets"),
        sessions_dir: base.join("sessions"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn defaults_are_stable() {
        let args = parse_args(&argv(&[])).expect("parse empty");
        assert_eq!(args.addr, "127.0.0.1:8600");
        assert_eq!(args.max_concurrent_imports, 2);
        assert_eq!(args.token, None);
        assert_eq!(args.user_data_dir, None);
    }

    #[test]
    fn inline_and_space_forms_agree() {
        let inline = parse_args(&argv(&["--addr=1.2.3.4:99", "--token=t"])).expect("inline form");
        let spaced = parse_args(&argv(&["--addr", "1.2.3.4:99", "--token", "t"])).expect("spaced form");
        assert_eq!(inline, spaced);
    }

    #[test]
    fn rejects_unknown_flag_missing_value_and_bad_concurrency() {
        assert!(parse_args(&argv(&["--nope"])).is_err(), "unknown flag");
        assert!(parse_args(&argv(&["--token"])).is_err(), "missing value");
        assert!(parse_args(&argv(&["--max-concurrent-imports", "0"])).is_err(), "zero");
        assert!(
            parse_args(&argv(&["--max-concurrent-imports", "many"])).is_err(),
            "not a number"
        );
    }

    #[test]
    fn user_data_dir_then_individual_overrides() {
        let args = parse_args(&argv(&["--user-data-dir", "/data"])).expect("parse");
        let paths = resolve_paths(&args);
        assert_eq!(paths.plugins_user, PathBuf::from("/data/plugins"));
        assert_eq!(paths.presets_dir, PathBuf::from("/data/presets"));
        assert_eq!(paths.sessions_dir, PathBuf::from("/data/sessions"));

        // 个别旗标覆盖 user-data-dir 的对应子目录，其余保留。
        let args = parse_args(&argv(&["--user-data-dir", "/data", "--presets-dir", "/other"])).expect("parse");
        let paths = resolve_paths(&args);
        assert_eq!(paths.presets_dir, PathBuf::from("/other"));
        assert_eq!(paths.plugins_user, PathBuf::from("/data/plugins"));
        assert_eq!(paths.sessions_dir, PathBuf::from("/data/sessions"));
    }
}
