//! `.absession` 会话文件（pipeline.md §5）：schema v1 序列化/反序列化、
//! 原子写盘、sha256 三态校验。
//!
//! 会话文件只存引用（路径 + 哈希 + 插件 id），**不缓存任何解析结果**
//! （PLAN.md §3.3 锁定决策）。序列化约定与协议一致（§5.1）：可选字段
//! 为空时整体省略键，禁止输出 `null` 或空容器；UTF-8 无 BOM。

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;

use ab_protocol::types::TimeRange;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// 会话文件 schema 版本（当前恒为 1）。
pub const SESSION_FILE_VERSION: u32 = 1;

/// 会话文件（pipeline.md §5.1）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionFile {
    /// 恒 1；读到更高版本拒绝打开并提示升级。
    pub version: u32,
    /// 导入文件清单，顺序即 UI 面板顺序。
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub files: Vec<SessionFileEntry>,
    /// file_id → 勾选上图的 metric id 列表。
    #[serde(skip_serializing_if = "HashMap::is_empty", default)]
    pub selected_metrics: HashMap<String, Vec<String>>,
    /// 图表视图状态。
    pub chart_view_state: ChartViewState,
    /// 游标位置（UTC 毫秒）；无游标时省略键。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor_ms: Option<i64>,
}

/// 会话文件条目（pipeline.md §5.1）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionFileEntry {
    /// 文件绝对路径。
    pub path: String,
    /// 导入时刻文件内容的 SHA-256，64 位小写十六进制。
    pub sha256: String,
    /// 导入时最终裁定（自动或手选）的插件 id。
    pub plugin_id: String,
}

/// 图表视图状态（pipeline.md §5.1）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChartViewState {
    /// dataZoom 视窗范围；省略 = 全量。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_range: Option<TimeRange>,
    /// 图例关闭的序列键（`file_id/metric`）。
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub legend_disabled: Vec<String>,
    /// 多 Y 轴模式。
    pub y_axis_scale: YAxisScale,
}

/// 多 Y 轴模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum YAxisScale {
    Shared,
    PerSeries,
}

/// 会话文件哈希校验三态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileVerifyStatus {
    /// 存在且 sha256 一致。
    Ok,
    /// 文件不存在（`sha256_of_file` 返回 NotFound）。
    Missing,
    /// 内容与记录不一致（含无法读取等不可校验情形）。
    Modified,
}

/// 会话文件打开错误。
#[derive(Debug)]
pub enum SessionFileError {
    Io(io::Error),
    Json(serde_json::Error),
    /// `version` 不为 1：读到的更高版本需提示用户升级。
    UnsupportedVersion(u32),
}

impl std::fmt::Display for SessionFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionFileError::Io(e) => write!(f, "session file io: {e}"),
            SessionFileError::Json(e) => write!(f, "session file json: {e}"),
            SessionFileError::UnsupportedVersion(v) => write!(
                f,
                "session file schema version {v} is newer than supported version {SESSION_FILE_VERSION}; please upgrade the app"
            ),
        }
    }
}

impl std::error::Error for SessionFileError {}

impl From<io::Error> for SessionFileError {
    fn from(e: io::Error) -> Self {
        SessionFileError::Io(e)
    }
}

impl From<serde_json::Error> for SessionFileError {
    fn from(e: serde_json::Error) -> Self {
        SessionFileError::Json(e)
    }
}

/// 原子写（pipeline.md §5.3）：先写 `<path>.tmp` 再 rename，防半文件。
/// Windows 的 `rename` 无法覆盖已存在目标，先移除旧文件再改名；任何失败
/// 均清理半成品 tmp，不触碰旧文件。
pub fn save_session(s: &SessionFile, path: &Path) -> io::Result<()> {
    let json = serde_json::to_vec_pretty(s).map_err(io::Error::other)?;
    let tmp = tmp_path(path);
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
    if path.exists() {
        fs::remove_file(path)?;
    }
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

fn tmp_path(path: &Path) -> std::path::PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".tmp");
    Path::new(&name).to_path_buf()
}

/// 读取并解析会话文件；`version > 1` 拒绝打开。
pub fn open_session(path: &Path) -> Result<SessionFile, SessionFileError> {
    let bytes = fs::read(path)?;
    let session: SessionFile = serde_json::from_slice(&bytes)?;
    if session.version != SESSION_FILE_VERSION {
        return Err(SessionFileError::UnsupportedVersion(session.version));
    }
    Ok(session)
}

/// 逐文件三态校验：`Ok` / `Missing` / `Modified`（pipeline.md §5.3 步骤 1-2）。
pub fn verify_files(s: &SessionFile) -> HashMap<String, FileVerifyStatus> {
    s.files
        .iter()
        .map(|entry| {
            let status = match sha256_of_file(Path::new(&entry.path)) {
                Ok(digest) if digest == entry.sha256 => FileVerifyStatus::Ok,
                Ok(_) => FileVerifyStatus::Modified,
                // NotFound（及任何不可读）→ Missing；不可校验视为哈希校验失败
                Err(e) if e.kind() == io::ErrorKind::NotFound => FileVerifyStatus::Missing,
                Err(_) => FileVerifyStatus::Modified,
            };
            (entry.path.clone(), status)
        })
        .collect()
}

/// 文件 SHA-256，64 位小写十六进制；流式读取不整载文件。
pub fn sha256_of_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    io::copy(&mut file, &mut hasher)?;
    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    Ok(out)
}
