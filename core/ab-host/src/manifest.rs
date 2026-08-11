//! plugin.json 加载与逐字段校验（host-runtime.md §2；protocol.md §7.2 / §7.3）。
//!
//! `Manifest` / `PluginEntry` / `MatchRules` 结构体由 `ab-protocol` 契约定义，
//! 本模块只实现宿主侧的执行版校验与入口路径解析。

use std::env;
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf, Prefix};

use ab_protocol::manifest::{Manifest, MatchRules};

/// 发现的错误类型（UI 插件管理页直接展示 reason）。
#[derive(Debug, Clone, PartialEq)]
pub enum DiscoveryError {
    /// 子文件夹根部缺 `plugin.json`。
    MissingManifest,
    /// 文件存在但不可读 / 非 UTF-8 可解码。
    Unreadable(String),
    /// 文件可解码但内容非法（非 JSON、非对象、缺必选字段、字段类型错）。
    JsonParse(String),
    /// `id` 不匹配 `^[a-z0-9][a-z0-9-_]{1,63}$`。
    InvalidId,
    /// 其余字段校验失败（display_name / version / match 等）。
    InvalidField(String),
    /// `entry` 形状非法（如 `command` 为空）。
    EntryError(String),
    /// `entry.command` 含路径分隔符但目标不存在；或解释器型入口 PATH 查找失败。
    EntryCommandNotFound,
    /// `entry.working_dir` 解析后不是已存在目录。
    EntryWorkingDirNotFound,
    /// `min_protocol_version` 大于宿主支持版本，需升级宿主。
    ProtocolVersionTooNew,
}

impl fmt::Display for DiscoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DiscoveryError::MissingManifest => write!(f, "plugin.json is missing"),
            DiscoveryError::Unreadable(e) => write!(f, "plugin.json unreadable: {e}"),
            DiscoveryError::JsonParse(e) => write!(f, "plugin.json is not a valid manifest: {e}"),
            DiscoveryError::InvalidId => {
                write!(f, "id must match ^[a-z0-9][a-z0-9-_]{{1,63}}$")
            }
            DiscoveryError::InvalidField(e) => write!(f, "invalid manifest field: {e}"),
            DiscoveryError::EntryError(e) => write!(f, "invalid entry: {e}"),
            DiscoveryError::EntryCommandNotFound => {
                write!(f, "entry.command could not be resolved (file missing)")
            }
            DiscoveryError::EntryWorkingDirNotFound => {
                write!(f, "entry.working_dir is not an existing directory")
            }
            DiscoveryError::ProtocolVersionTooNew => {
                write!(
                    f,
                    "plugin requires a newer host (min_protocol_version too high)"
                )
            }
        }
    }
}

impl std::error::Error for DiscoveryError {}

/// 解析后的启动入口（§2.2；协议 `PluginEntry` 的宿主侧可执行形态）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedEntry {
    /// `command` 解析后的绝对路径（解释器型则为 PATH 查找结果路径）。
    pub program: PathBuf,
    /// 命令行参数。
    pub args: Vec<String>,
    /// 解析后的绝对工作目录。
    pub working_dir: PathBuf,
}

/// 加载子文件夹根部的 `plugin.json`（§2.1「文件本身」行）。
pub fn load_manifest(dir: &Path) -> Result<Manifest, DiscoveryError> {
    let path = dir.join("plugin.json");
    let raw = match fs::read(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(DiscoveryError::MissingManifest);
        }
        Err(e) => return Err(DiscoveryError::Unreadable(e.to_string())),
    };
    let text = String::from_utf8(raw)
        .map_err(|e| DiscoveryError::Unreadable(format!("plugin.json is not valid UTF-8: {e}")))?;
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| DiscoveryError::JsonParse(e.to_string()))?;
    if !value.is_object() {
        return Err(DiscoveryError::JsonParse(
            "top-level value is not a JSON object".to_string(),
        ));
    }
    serde_json::from_value(value)
        .map_err(|e| DiscoveryError::JsonParse(format!("manifest schema violation: {e}")))
}

/// 逐字段校验（§2.1 字段表 + §2.3 `match` 形状校验）。
///
/// 校验失败项进 [`crate::discovery::InvalidPlugin`]，列出但不拉起。
pub fn validate(m: &Manifest) -> Result<(), DiscoveryError> {
    // id：`^[a-z0-9][a-z0-9-_]{1,63}$`（全部 ASCII，字节数即字符数）。
    let mut chars = m.id.chars();
    let first = chars.next();
    if !matches!(first, Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit())
        || !(2..=64).contains(&m.id.len())
        || !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    {
        return Err(DiscoveryError::InvalidId);
    }

    if m.display_name.is_empty() {
        return Err(DiscoveryError::InvalidField(
            "display_name is empty".to_string(),
        ));
    }

    if semver::Version::parse(&m.version).is_err() {
        return Err(DiscoveryError::InvalidField(format!(
            "version is not valid semver: {:?}",
            m.version
        )));
    }

    if m.entry.command.trim().is_empty() {
        return Err(DiscoveryError::EntryError("command is empty".to_string()));
    }

    validate_match_rules(&m.r#match)?;

    if m.min_protocol_version > ab_protocol::PROTOCOL_VERSION {
        return Err(DiscoveryError::ProtocolVersionTooNew);
    }

    Ok(())
}

/// `match` 形状校验（§2.3）：扩展名可空、逐项转小写去前导点后不得含路径分隔符；
/// 指纹元素非空 string。形状合法时同步返回规范化后的规则（扫描流程就地落盘）。
fn validate_match_rules(rules: &MatchRules) -> Result<(), DiscoveryError> {
    for raw in &rules.extensions {
        let ext = raw.trim_start_matches('.');
        if ext.is_empty()
            || ext.chars().any(|c| c == '/' || c == '\\')
            || !ext
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "+#-_".contains(c))
        {
            return Err(DiscoveryError::InvalidField(format!(
                "match.extensions contains an invalid entry: {raw:?}"
            )));
        }
    }
    if let Some(fingerprints) = &rules.header_fingerprints {
        if fingerprints.iter().any(|f| f.is_empty()) {
            return Err(DiscoveryError::InvalidField(
                "match.header_fingerprints entries must be non-empty".to_string(),
            ));
        }
    }
    Ok(())
}

/// 规范化 `match` 规则：扩展名转小写、去前导点（§2.3），供 B 路调度直接消费。
/// `validate` 通过后调用；被拒绝的 manifest 不进入规范化路径。
pub fn normalize_match_rules(rules: &mut MatchRules) {
    for ext in &mut rules.extensions {
        *ext = ext.trim_start_matches('.').to_ascii_lowercase();
    }
}

/// `entry` 校验与路径解析（§2.2，对应 protocol.md §7.3）。
///
/// - `command` 含路径分隔符 → 一律相对 `plugin.json` 所在目录解析为绝对路径，
///   必须存在且为文件；绝对路径直接使用；
/// - 不含路径分隔符 → 解释器型入口（如 `python`），按系统约定经 PATH / PATHEXT 查找；
/// - `working_dir` 省略时默认 = `plugin.json` 所在目录。
pub fn resolve_entry(m: &Manifest, plugin_dir: &Path) -> Result<ResolvedEntry, DiscoveryError> {
    let command = m.entry.command.trim();
    let program = if Path::new(command).is_absolute() {
        let p = PathBuf::from(command);
        if !p.is_file() {
            return Err(DiscoveryError::EntryCommandNotFound);
        }
        p
    } else if command.contains('/') || command.contains('\\') {
        let p = plugin_dir.join(command);
        p.canonicalize()
            .map(simplify_canonical)
            .map_err(|_| DiscoveryError::EntryCommandNotFound)?
    } else {
        find_in_path(command).ok_or(DiscoveryError::EntryCommandNotFound)?
    };

    let working_dir = match &m.entry.working_dir {
        Some(wd) => {
            let p = if Path::new(wd).is_absolute() {
                PathBuf::from(wd)
            } else {
                plugin_dir.join(wd)
            };
            p.canonicalize()
                .map(simplify_canonical)
                .ok()
                .filter(|p| p.is_dir())
                .ok_or(DiscoveryError::EntryWorkingDirNotFound)?
        }
        None => plugin_dir
            .canonicalize()
            .map(simplify_canonical)
            .map_err(|_| DiscoveryError::EntryWorkingDirNotFound)?,
    };

    Ok(ResolvedEntry {
        program,
        args: m.entry.args.clone(),
        working_dir,
    })
}

/// 解释器型入口的 PATH / PATHEXT 查找（Windows 约定；非 Windows 平台仅按 PATH 逐名试）。
fn find_in_path(command: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    let pathext: Vec<String> = env::var("PATHEXT")
        .map(|v| {
            v.split(';')
                .filter(|s| !s.is_empty())
                .map(|s| s.to_ascii_lowercase())
                .collect()
        })
        .unwrap_or_else(|_| vec![".com".into(), ".exe".into(), ".bat".into(), ".cmd".into()]);
    for dir in env::split_paths(&path) {
        let candidate = dir.join(command);
        if candidate.is_file() {
            return candidate.canonicalize().ok().map(simplify_canonical);
        }
        for ext in &pathext {
            let mut with_ext = candidate.clone();
            with_ext.set_extension(ext.trim_start_matches('.'));
            if with_ext.is_file() {
                return with_ext.canonicalize().ok().map(simplify_canonical);
            }
        }
    }
    None
}

/// 剥离 Windows 上 `fs::canonicalize` 产出的 `\\?\` 前缀（含 UNC 形式
/// `\\?\UNC\server\share`），还原为常规路径形式。
///
/// CreateProcess 虽可直接消费 verbatim 路径，但该路径会用作插件
/// `program` / `working_dir`：经 cmd.exe 拉起 .bat/.cmd、UI 展示比较、
/// 第三方解释器拼接参数时可能不识别前缀，故统一剥离。其余前缀
/// （普通盘符、普通 UNC）与 verbatim 设备路径原样保留。
///
/// 仅用 std 实现：`Path::strip_prefix` 无法剥离 verbatim 前缀（Prefix
/// 组件连同盘符/服务器名整体比较），故按 `PrefixComponent::kind()`
/// 重建为普通前缀后拼回剩余组件。
#[cfg(windows)]
fn simplify_canonical(path: PathBuf) -> PathBuf {
    let mut comps = path.components();
    let prefix = match comps.next() {
        Some(Component::Prefix(pc)) => pc,
        // 无前缀（相对路径等）或非 Windows 风格路径，原样返回。
        _ => return path,
    };
    let rebuilt = match prefix.kind() {
        // `\\?\C:` → `C:\`（带根分隔符；`C:` 单独存在时是盘相对路径，语义不同）
        Prefix::VerbatimDisk(d) => PathBuf::from(format!("{}:\\", d as char)),
        // `\\?\UNC\server\share` → `\\server\share`
        Prefix::VerbatimUNC(server, share) => PathBuf::from(format!(
            "\\\\{}\\{}",
            server.to_string_lossy(),
            share.to_string_lossy()
        )),
        // 其余前缀（普通盘符、普通 UNC、设备命名空间）不动。
        _ => return path,
    };
    // 剩余组件以 RootDir 开头时 `as_path` 返回 `\...`：VerbatimDisk 重建已含
    // 根分隔符，直接拼尾（join 会在两路径都带分隔符时重复，这里用
    // push 的字符串拼接路径前先去掉剩余串的前导分隔符）。
    let tail = comps.as_path().to_path_buf();
    let tail_str = tail.to_string_lossy();
    let tail_str = tail_str.strip_prefix('\\').unwrap_or(&tail_str);
    let rebuilt = rebuilt.join(tail_str);
    // 长路径守卫：重建后长度超过 MAX_PATH(260) 的普通路径会被 is_file /
    // CreateProcess 等非 verbatim API 拒绝（应用清单未声明 longPathAware），
    // 此时保留 verbatim 前缀（CreateProcess 可直接消费）。
    if rebuilt.as_os_str().to_string_lossy().len() > 260 {
        return path;
    }
    rebuilt
}

/// 非 Windows 平台无 verbatim 前缀问题，原样返回。
#[cfg(not(windows))]
fn simplify_canonical(path: PathBuf) -> PathBuf {
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use ab_protocol::manifest::{MatchRules, PluginEntry};

    fn manifest() -> Manifest {
        Manifest {
            id: "demo-tool".to_string(),
            display_name: "Demo Tool".to_string(),
            version: "0.1.0".to_string(),
            entry: PluginEntry {
                command: "run.exe".to_string(),
                args: vec!["--stdio".to_string()],
                working_dir: None,
            },
            r#match: MatchRules {
                extensions: vec!["csv".to_string()],
                header_fingerprints: None,
            },
            min_protocol_version: 1,
        }
    }

    #[test]
    fn id_regex_is_enforced() {
        for (id, ok) in [
            ("demo-tool", true),
            ("a", false),
            ("0abc", true),
            ("A-bad", false),
            ("has space", false),
            ("bad/char", false),
            ("under_score", true),
            (
                "x-1234567890-abcdefghijklmnopqrstuvwxyz-0123456789-ABCDEF",
                false,
            ),
        ] {
            let mut m = manifest();
            m.id = id.to_string();
            assert_eq!(validate(&m).is_ok(), ok, "id {id:?}");
        }
    }

    #[test]
    fn version_and_display_name_checked() {
        let mut m = manifest();
        m.version = "not-semver".to_string();
        assert!(matches!(validate(&m), Err(DiscoveryError::InvalidField(_))));

        let mut m = manifest();
        m.display_name = String::new();
        assert!(matches!(validate(&m), Err(DiscoveryError::InvalidField(_))));
    }

    #[test]
    fn protocol_too_new_rejected() {
        let mut m = manifest();
        m.min_protocol_version = 2;
        assert_eq!(validate(&m), Err(DiscoveryError::ProtocolVersionTooNew));
    }

    #[test]
    fn extensions_normalized_and_illegal_rejected() {
        let mut m = manifest();
        m.r#match.extensions = vec![".CSV".to_string(), "txt".to_string()];
        validate(&m).expect("valid");
        normalize_match_rules(&mut m.r#match);
        assert_eq!(m.r#match.extensions, ["csv", "txt"]);

        let mut m = manifest();
        m.r#match.extensions = vec!["a/b".to_string()];
        assert!(matches!(validate(&m), Err(DiscoveryError::InvalidField(_))));

        let mut m = manifest();
        m.r#match.extensions = vec![String::new()];
        assert!(matches!(validate(&m), Err(DiscoveryError::InvalidField(_))));
    }

    #[cfg(windows)]
    #[test]
    fn simplify_canonical_strips_verbatim_disk_prefix() {
        // 普通盘符：`\\?\C:\...` → `C:\...`
        let plain = simplify_canonical(PathBuf::from(r"\\?\C:\logs\run.exe"));
        assert_eq!(plain, PathBuf::from(r"C:\logs\run.exe"));

        // 真实 canonicalize 结果剥离后无前缀且指向同一路径
        let dir = env::temp_dir();
        let canon = fs::canonicalize(&dir).expect("temp dir 应可规范化");
        let cleaned = simplify_canonical(canon);
        let s = cleaned.to_string_lossy();
        assert!(!s.starts_with("\\\\?\\"), "剥离后不得含 verbatim 前缀: {s}");
        assert_eq!(cleaned, dir.canonicalize().map(simplify_canonical).unwrap());
        assert!(cleaned.is_dir(), "剥离后路径仍可被 fs API 消费");
    }

    #[cfg(windows)]
    #[test]
    fn simplify_canonical_strips_verbatim_unc_prefix() {
        // UNC：`\\?\UNC\server\share\...` → `\\server\share\...`
        let unc = simplify_canonical(PathBuf::from(r"\\?\UNC\server\share\logs\a.csv"));
        assert_eq!(unc, PathBuf::from(r"\\server\share\logs\a.csv"));

        // 仅到 share 层也成立
        let bare = simplify_canonical(PathBuf::from(r"\\?\UNC\host\data"));
        assert_eq!(bare, PathBuf::from(r"\\host\data"));
    }

    #[cfg(windows)]
    #[test]
    fn simplify_canonical_preserves_normal_paths() {
        // 常规盘符路径与常规 UNC 路径不受影响
        let plain = PathBuf::from(r"C:\Windows\notepad.exe");
        assert_eq!(simplify_canonical(plain.clone()), plain);
        let unc = PathBuf::from(r"\\server\share\f.txt");
        assert_eq!(simplify_canonical(unc.clone()), unc);
    }

    #[cfg(windows)]
    #[test]
    fn simplify_canonical_keeps_verbatim_for_overlong_paths() {
        // 重建后长度 > MAX_PATH(260) 的路径必须保留 verbatim 前缀，
        // 否则 is_file/CreateProcess 会因路径过长而拒绝（长路径守卫）。
        let dir = "C".to_string();
        let mut long = String::new();
        let mut tail = String::new();
        for _ in 0..30 {
            tail.push_str("\\component-name");
        }
        long.push_str(r"\\?\");
        long.push_str(&dir);
        long.push(':');
        long.push_str(&tail);
        assert!(
            long.len() > 260,
            "fixture 长度应超过 MAX_PATH: {}",
            long.len()
        );

        let kept = simplify_canonical(PathBuf::from(long.clone()));
        assert_eq!(kept, PathBuf::from(long), "超长路径应保留 verbatim 前缀");

        // 短路径仍正常剥离（不回归）
        let short = simplify_canonical(PathBuf::from(r"\\?\C:\logs\run.exe"));
        assert_eq!(short, PathBuf::from(r"C:\logs\run.exe"));
    }

    #[cfg(windows)]
    #[test]
    fn resolve_entry_relative_command_has_no_verbatim_prefix() {
        // 覆盖 resolve_entry 的 canonicalize 产出路径（修复点）
        let base = env::temp_dir().join(format!("ab-host-entry-{}", std::process::id()));
        fs::create_dir_all(&base).unwrap();
        let exe = base.join("run.exe");
        fs::write(&exe, b"MZ").unwrap();

        let mut m = manifest();
        m.entry.command = "./run.exe".to_string();
        let resolved = resolve_entry(&m, &base).expect("相对 command 应解析成功");
        let s = resolved.program.to_string_lossy();
        assert!(
            !s.starts_with("\\\\?\\"),
            "program 不得带 verbatim 前缀: {s}"
        );
        assert!(resolved.program.is_file());
        // working_dir 缺省 = plugin.json 所在目录，同样不得带前缀
        let wd = resolved.working_dir.to_string_lossy();
        assert!(
            !wd.starts_with("\\\\?\\"),
            "working_dir 不得带 verbatim 前缀: {wd}"
        );

        fs::remove_dir_all(&base).unwrap();
    }
}
