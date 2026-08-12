//! 更新检查网络抽象（task-3-brief.md）：GitHub Releases 查询 / 资产下载。
//!
//! [`UpdateFetcher`] 为更新链路（任务 6）唯一网络入口；生产实现
//! [`GitHubFetcher`]（reqwest + rustls，30s 请求超时）消费
//! `GET /repos/{owner}/{repo}/releases/latest`，测试实现
//! [`MockFetcher`] 提供确定性往返。纯函数
//! [`parse_repo_url`] / [`tag_to_version`] / [`select_zip_asset`]
//! 与 Mock 均为同步单元测试覆盖（见文件尾部 `tests`）。

use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

/// 发行版信息（任务 6 更新流程消费方数据形状）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseInfo {
    /// GitHub tag（如 `v1.2.0`）。
    pub tag_name: String,
    /// 选定 zip 资产的下载地址。
    pub asset_url: String,
    /// 选定 zip 资产的文件名。
    pub asset_name: String,
}

/// GitHub Releases 资产（直接承接 API JSON 反序列化；未知字段默认忽略）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct ReleaseAsset {
    pub name: String,
    pub browser_download_url: String,
}

/// 更新检查错误（Display 实现见下）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateError {
    /// 仓库引用无法解析（[`parse_repo_url`] 拒绝形态）。
    RepoParse,
    /// 网络层失败（连接 / 传输 / 本地 IO / URL 校验拒绝等）。
    Network(String),
    /// 未恰好命中一个 zip 资产；载荷为 zip 资产个数。
    NoZipAsset(usize),
    /// GitHub API 非 2xx；载荷为状态码文本。
    Api(String),
    /// 资产体积超过 [`MAX_ASSET_BYTES`] 上限（Content-Length 预检或
    /// 流式累计超限），下载中止且已写临时文件已删除。
    TooLarge,
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UpdateError::RepoParse => write!(f, "invalid repository reference"),
            UpdateError::Network(msg) => write!(f, "network error: {msg}"),
            UpdateError::NoZipAsset(n) => write!(f, "expected exactly one .zip asset, found {n}"),
            UpdateError::Api(msg) => write!(f, "GitHub API error: {msg}"),
            UpdateError::TooLarge => write!(
                f,
                "asset download exceeds size limit ({} MiB)",
                MAX_ASSET_BYTES / 1024 / 1024
            ),
        }
    }
}

impl std::error::Error for UpdateError {}

/// 更新检查抽象：查询最新发行版 + 下载资产。
///
/// 全部更新逻辑经由本 trait 进出，测试以 [`MockFetcher`] 驱动。
#[async_trait::async_trait]
pub trait UpdateFetcher: Send + Sync {
    /// 查询 `owner/repo` 最新发行版（选中 zip 资产）。
    async fn fetch_latest_release(
        &self,
        owner: &str,
        repo: &str,
    ) -> Result<ReleaseInfo, UpdateError>;

    /// 将 `url` 处资产下载到 `dest`。
    async fn download(&self, url: &str, dest: &Path) -> Result<(), UpdateError>;
}

/// 解析仓库引用：接受 `https://github.com/o/r` 与裸 `o/r`，拒绝其余形态
/// （含 `.git` 后缀、query/fragment——终审修复：GitHub API 路径不接受这些）。
pub fn parse_repo_url(url: &str) -> Option<(String, String)> {
    let trimmed = url.trim();
    let rest = if let Some(rest) = trimmed.strip_prefix("https://github.com/") {
        rest
    } else if !trimmed.contains("://") && !trimmed.starts_with('/') {
        trimmed
    } else {
        return None;
    };
    let rest = rest.trim_end_matches('/');
    if rest.ends_with(".git") || rest.contains('?') || rest.contains('#') {
        return None;
    }
    let (owner, repo) = rest.split_once('/')?;
    if owner.is_empty() || repo.is_empty() || owner.contains('/') || repo.contains('/') {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

/// tag → semver：剥离 `v` 前缀后解析，非 semver 返回 `None`。
pub fn tag_to_version(tag: &str) -> Option<semver::Version> {
    let without_prefix = tag.strip_prefix('v').unwrap_or(tag);
    semver::Version::parse(without_prefix).ok()
}

/// 从资产表中选出恰好一个 zip 资产；个数不为 1 时
/// `Err(UpdateError::NoZipAsset(zip 个数))`。
pub fn select_zip_asset(assets: &[ReleaseAsset]) -> Result<&ReleaseAsset, UpdateError> {
    let zips: Vec<&ReleaseAsset> = assets
        .iter()
        .filter(|asset| asset.name.ends_with(".zip"))
        .collect();
    match zips.len() {
        1 => Ok(zips[0]),
        n => Err(UpdateError::NoZipAsset(n)),
    }
}

/// 测试实现：fetch 按 FIFO 先弹错误队列（`errors`），再弹发行版队列
/// （空队列 → `Network` 错误）；download 在 `download_payload` 有值时复制
/// 该文件到 `dest`（更新流集成测试注入真实 ZIP 用），否则落空文件；
/// 两种路径都记录 `(url, dest)` 调用。
#[derive(Debug, Default)]
pub struct MockFetcher {
    /// 待弹错误队列（前端弹出；任务 6 注入 `NoZipAsset` 等确定性失败）。
    pub errors: Mutex<Vec<UpdateError>>,
    /// 待弹发行版队列（前端弹出）。
    pub releases: Mutex<Vec<ReleaseInfo>>,
    /// 已记录下载调用。
    pub downloads: Mutex<Vec<(String, PathBuf)>>,
    /// 下载载荷源文件（设置后 download 复制到 dest 代替落空文件）。
    pub download_payload: Mutex<Option<PathBuf>>,
}

#[async_trait::async_trait]
impl UpdateFetcher for MockFetcher {
    async fn fetch_latest_release(
        &self,
        _owner: &str,
        _repo: &str,
    ) -> Result<ReleaseInfo, UpdateError> {
        let mut errors = self.errors.lock().unwrap();
        if !errors.is_empty() {
            return Err(errors.remove(0));
        }
        drop(errors);
        let mut queue = self.releases.lock().unwrap();
        if queue.is_empty() {
            return Err(UpdateError::Network("mock queue empty".to_string()));
        }
        Ok(queue.remove(0))
    }

    async fn download(&self, url: &str, dest: &Path) -> Result<(), UpdateError> {
        self.downloads
            .lock()
            .unwrap()
            .push((url.to_string(), dest.to_path_buf()));
        let payload = self.download_payload.lock().unwrap().clone();
        match payload {
            Some(src) => std::fs::copy(&src, dest)
                .map(|_| ())
                .map_err(|e| UpdateError::Network(e.to_string())),
            None => std::fs::write(dest, b"").map_err(|e| UpdateError::Network(e.to_string())),
        }
    }
}

/// 单资产体积上限（500 MiB；服务端 Content-Length 超限即拒绝）。
const MAX_ASSET_BYTES: u64 = 500 * 1024 * 1024;

/// 下载 URL 预检：仅接受 https（拒绝 http/file 等与不可解析形态）。
/// 初始 URL 与重定向后最终 URL 共用（重定向终点也必须保持 https）。
fn validate_download_url(url: &str) -> Result<(), UpdateError> {
    let parsed = reqwest::Url::parse(url)
        .map_err(|_| UpdateError::Network(format!("invalid asset URL: {url}")))?;
    if parsed.scheme() != "https" {
        return Err(UpdateError::Network(format!(
            "asset URL scheme `{}` not allowed (https required): {url}",
            parsed.scheme()
        )));
    }
    Ok(())
}

/// 响应预检（下载任何字节前）：重定向后最终 URL 必须仍是 https；
/// Content-Length 超限立即拒绝（不做任何落盘）；content-type 白名单
/// （`application/zip` / `application/octet-stream`，GitHub 资产实际值；
/// 缺失时容忍并继续流式校验）。参数段与大小写忽略。
fn validate_download_response(
    headers: &reqwest::header::HeaderMap,
    final_url: &str,
    content_length: Option<u64>,
) -> Result<(), UpdateError> {
    validate_download_url(final_url)?;
    if let Some(len) = content_length {
        if len > MAX_ASSET_BYTES {
            return Err(UpdateError::TooLarge);
        }
    }
    let Some(value) = headers.get(reqwest::header::CONTENT_TYPE) else {
        return Ok(());
    };
    let Ok(ct) = value.to_str() else {
        return Ok(());
    };
    let base = ct.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    match base.as_str() {
        "application/zip" | "application/octet-stream" => Ok(()),
        other => Err(UpdateError::Network(format!(
            "asset content-type `{other}` not allowed (expected application/zip or \
             application/octet-stream)"
        ))),
    }
}

/// 下载 chunk 流源抽象：生产侧以 reqwest `Response` 适配
/// （[`ResponseChunkSource`]），测试侧以内存块队列模拟 chunked 流，
/// 无需真实网络即可覆盖流式写入路径。
#[async_trait::async_trait]
pub(crate) trait ChunkSource: Send {
    /// 取下一块；`Ok(None)` 表示流结束；错误透传为 [`UpdateError`]。
    async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, UpdateError>;
}

/// 有界下载辅助：把 [`ChunkSource`] 流逐块写入 `dest`，累计字节超过
/// `limit` 立即中止（删除已写临时文件）并返回 [`UpdateError::TooLarge`]；
/// 任何失败（写错误 / 源错误 / fsync 失败）同样删除临时文件。
/// 与网络解耦：生产与测试共用同一写入路径。
pub(crate) struct BoundedDownload<C: ChunkSource> {
    source: C,
    dest: PathBuf,
    limit: u64,
}

impl<C: ChunkSource> BoundedDownload<C> {
    pub(crate) fn new(source: C, dest: impl Into<PathBuf>, limit: u64) -> Self {
        Self {
            source,
            dest: dest.into(),
            limit,
        }
    }

    /// 流式写入 + 计数 + 超限中止 + 失败清理；返回实际写入字节数。
    pub(crate) async fn run(mut self) -> Result<u64, UpdateError> {
        if let Some(parent) = self.dest.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| UpdateError::Network(e.to_string()))?;
            }
        }
        let mut file =
            std::fs::File::create(&self.dest).map_err(|e| UpdateError::Network(e.to_string()))?;
        let outcome = self.write_stream(&mut file).await;
        let written = match outcome {
            Ok(written) => written,
            Err(err) => return Err(self.cleanup(file, err)),
        };
        if let Err(e) = file.sync_all() {
            return Err(self.cleanup(file, UpdateError::Network(e.to_string())));
        }
        Ok(written)
    }

    async fn write_stream(&mut self, file: &mut std::fs::File) -> Result<u64, UpdateError> {
        let mut written: u64 = 0;
        while let Some(chunk) = self.source.next_chunk().await? {
            // 写前判定：本块会越限则不再落任何字节，直接中止。
            if written.saturating_add(chunk.len() as u64) > self.limit {
                return Err(UpdateError::TooLarge);
            }
            file.write_all(&chunk)
                .map_err(|e| UpdateError::Network(e.to_string()))?;
            written += chunk.len() as u64;
        }
        Ok(written)
    }

    /// 先释放文件句柄再删除临时文件（Windows 需要句柄关闭后才能删除），
    /// 返回原错误。
    fn cleanup(&self, file: std::fs::File, err: UpdateError) -> UpdateError {
        drop(file);
        let _ = std::fs::remove_file(&self.dest);
        err
    }
}

/// reqwest `Response` 的 [`ChunkSource`] 适配：逐块读取（`chunk()` 在
/// 每次成功读取后重置 30s 读超时）。
struct ResponseChunkSource<'a> {
    response: &'a mut reqwest::Response,
}

#[async_trait::async_trait]
impl ChunkSource for ResponseChunkSource<'_> {
    async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, UpdateError> {
        self.response
            .chunk()
            .await
            .map(|chunk| chunk.map(|bytes| bytes.to_vec()))
            .map_err(|e| UpdateError::Network(e.to_string()))
    }
}

/// GitHub Releases 生产实现：reqwest + rustls，30s 请求超时。
#[derive(Debug, Clone)]
pub struct GitHubFetcher {
    client: reqwest::Client,
}

impl GitHubFetcher {
    /// 构造客户端（30s 无数据超时：每次成功读取后重置；重定向最多 5 跳，
    /// 收紧默认 10 跳）。
    pub fn new() -> Result<Self, UpdateError> {
        let client = reqwest::Client::builder()
            .read_timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .map_err(|e| UpdateError::Network(e.to_string()))?;
        Ok(Self { client })
    }
}

/// GitHub `releases/latest` 载荷（仅反序列化所需字段）。
#[derive(serde::Deserialize)]
struct GithubRelease {
    tag_name: String,
    assets: Vec<ReleaseAsset>,
}

#[async_trait::async_trait]
impl UpdateFetcher for GitHubFetcher {
    async fn fetch_latest_release(
        &self,
        owner: &str,
        repo: &str,
    ) -> Result<ReleaseInfo, UpdateError> {
        let url = format!("https://api.github.com/repos/{owner}/{repo}/releases/latest");
        let user_agent = format!("AnalysisBuddy/{}", env!("CARGO_PKG_VERSION"));
        let response = self
            .client
            .get(&url)
            .header(reqwest::header::USER_AGENT, user_agent)
            .send()
            .await
            .map_err(|e| UpdateError::Network(e.to_string()))?;
        if !response.status().is_success() {
            return Err(UpdateError::Api(response.status().to_string()));
        }
        let payload: GithubRelease = response
            .json()
            .await
            .map_err(|e| UpdateError::Network(e.to_string()))?;
        let asset = select_zip_asset(&payload.assets)?;
        Ok(ReleaseInfo {
            tag_name: payload.tag_name,
            asset_url: asset.browser_download_url.clone(),
            asset_name: asset.name.clone(),
        })
    }

    async fn download(&self, url: &str, dest: &Path) -> Result<(), UpdateError> {
        // URL scheme 预检：非 https（http/file 等）在发起任何请求前拒绝。
        validate_download_url(url)?;
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| UpdateError::Network(e.to_string()))?;
        if !response.status().is_success() {
            return Err(UpdateError::Api(response.status().to_string()));
        }
        // 响应预检：重定向后最终 URL 必须仍是 https（重定向策略见
        // `GitHubFetcher::new`，limited(5)）；Content-Length 超限 /
        // content-type 非白名单不落任何字节。
        validate_download_response(
            response.headers(),
            response.url().as_str(),
            response.content_length(),
        )?;
        // workspace tokio 未启用 `fs` feature，此处同步写文件；有界流式
        // 逐块落盘（BoundedDownload：累计超 MAX_ASSET_BYTES 立即中止并
        // 删除已写临时文件），不整包驻留内存。
        let mut response = response;
        BoundedDownload::new(
            ResponseChunkSource {
                response: &mut response,
            },
            dest,
            MAX_ASSET_BYTES,
        )
        .run()
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    fn asset(name: &str, url: &str) -> ReleaseAsset {
        ReleaseAsset {
            name: name.to_string(),
            browser_download_url: url.to_string(),
        }
    }

    /// `https://github.com/o/r` 形态。
    #[test]
    fn parse_repo_url_accepts_github_https() {
        assert_eq!(
            parse_repo_url("https://github.com/o/r"),
            Some(("o".to_string(), "r".to_string()))
        );
    }

    /// 裸 `o/r` 形态。
    #[test]
    fn parse_repo_url_accepts_bare_owner_repo() {
        assert_eq!(
            parse_repo_url("o/r"),
            Some(("o".to_string(), "r".to_string()))
        );
    }

    /// 尾斜杠容忍。
    #[test]
    fn parse_repo_url_accepts_trailing_slash() {
        assert_eq!(
            parse_repo_url("https://github.com/o/r/"),
            Some(("o".to_string(), "r".to_string()))
        );
    }

    /// 其余形态一律拒绝：缺 owner/repo、多段路径、非 https、非 github.com、
    /// `.git` 后缀、query/fragment。
    #[test]
    fn parse_repo_url_rejects_other_shapes() {
        for url in [
            "",
            "o",
            "o/r/x",
            "/o/r",
            "github.com/o/r",
            "http://github.com/o/r",
            "https://gitlab.com/o/r",
            "https://github.com/",
            "https://github.com/o/r.git",
            "o/r.git",
            "https://github.com/o/r.git/",
            "https://github.com/o/r?tab=releases",
            "o/r?tab=releases",
            "https://github.com/o/r#readme",
        ] {
            assert_eq!(parse_repo_url(url), None, "should reject {url:?}");
        }
    }

    /// `v` 前缀剥离后按 semver 解析。
    #[test]
    fn tag_to_version_strips_v_prefix() {
        assert_eq!(
            tag_to_version("v1.2.0"),
            Some(semver::Version::new(1, 2, 0))
        );
    }

    /// 无前缀版本直接解析。
    #[test]
    fn tag_to_version_accepts_plain_version() {
        assert_eq!(tag_to_version("1.2.0"), Some(semver::Version::new(1, 2, 0)));
    }

    /// 非 semver 标签 → None。
    #[test]
    fn tag_to_version_rejects_non_semver() {
        assert_eq!(tag_to_version("abc"), None);
        assert_eq!(tag_to_version("v1.2"), None);
    }

    /// 恰好一个 zip 资产 → Ok。
    #[test]
    fn select_zip_asset_single_zip_is_ok() {
        let assets = vec![
            asset("AnalysisBuddy-1.2.0.zip", "https://example.com/a.zip"),
            asset("checksums.txt", "https://example.com/checksums.txt"),
        ];
        let picked = select_zip_asset(&assets).expect("single zip");
        assert_eq!(picked.name, "AnalysisBuddy-1.2.0.zip");
    }

    /// 多个 zip 资产 → Err(NoZipAsset(个数))。
    #[test]
    fn select_zip_asset_multiple_zips_reports_count() {
        let assets = vec![
            asset("a.zip", "https://example.com/a.zip"),
            asset("b.zip", "https://example.com/b.zip"),
        ];
        assert!(matches!(
            select_zip_asset(&assets),
            Err(UpdateError::NoZipAsset(2))
        ));
    }

    /// 无 zip 资产 → Err(NoZipAsset(0))。
    #[test]
    fn select_zip_asset_no_zip_reports_zero() {
        let assets = vec![
            asset("a.tar.gz", "https://example.com/a.tar.gz"),
            asset("b.exe", "https://example.com/b.exe"),
        ];
        assert!(matches!(
            select_zip_asset(&assets),
            Err(UpdateError::NoZipAsset(0))
        ));
    }

    /// Mock 往返：fetch 按 FIFO 弹第一个，download 落空文件并记录调用。
    #[tokio::test]
    async fn mock_fetcher_round_trip() {
        let fetcher = MockFetcher::default();
        fetcher.releases.lock().unwrap().push(ReleaseInfo {
            tag_name: "v1.2.0".to_string(),
            asset_url: "https://example.com/a.zip".to_string(),
            asset_name: "a.zip".to_string(),
        });
        fetcher.releases.lock().unwrap().push(ReleaseInfo {
            tag_name: "v1.1.0".to_string(),
            asset_url: "https://example.com/b.zip".to_string(),
            asset_name: "b.zip".to_string(),
        });

        let first = fetcher.fetch_latest_release("o", "r").await.expect("first");
        assert_eq!(first.tag_name, "v1.2.0");
        let second = fetcher
            .fetch_latest_release("o", "r")
            .await
            .expect("second");
        assert_eq!(second.tag_name, "v1.1.0");

        let dest = std::env::temp_dir().join(format!("ab-app-mock-dl-{}.bin", std::process::id()));
        fetcher
            .download("https://example.com/a.zip", &dest)
            .await
            .expect("download");
        let downloads = fetcher.downloads.lock().unwrap();
        assert_eq!(downloads.len(), 1);
        assert_eq!(downloads[0].0, "https://example.com/a.zip");
        assert_eq!(downloads[0].1, dest);
        drop(downloads);
        assert_eq!(std::fs::read(&dest).expect("dest file"), b"");
        let _ = std::fs::remove_file(&dest);
    }

    /// 空 mock 队列 → Network 错误（与真实实现一致：无可查发行版）。
    #[tokio::test]
    async fn mock_fetcher_empty_releases_is_network_error() {
        let fetcher = MockFetcher::default();
        assert!(matches!(
            fetcher.fetch_latest_release("o", "r").await,
            Err(UpdateError::Network(_))
        ));
    }

    /// 错误队列先于发行版队列弹出（任务 6：注入 NoZipAsset 等确定性失败）。
    #[tokio::test]
    async fn mock_fetcher_errors_popped_before_releases() {
        let fetcher = MockFetcher::default();
        fetcher
            .errors
            .lock()
            .unwrap()
            .push(UpdateError::NoZipAsset(2));
        fetcher.releases.lock().unwrap().push(ReleaseInfo {
            tag_name: "v1.2.0".to_string(),
            asset_url: "https://example.com/a.zip".to_string(),
            asset_name: "a.zip".to_string(),
        });
        assert!(matches!(
            fetcher.fetch_latest_release("o", "r").await,
            Err(UpdateError::NoZipAsset(2))
        ));
        let next = fetcher
            .fetch_latest_release("o", "r")
            .await
            .expect("release");
        assert_eq!(next.tag_name, "v1.2.0");
    }

    /// download 在注入 payload 时复制文件内容（更新流集成测试：mock 下载
    /// 后走真实解压安装管线需要 dest 为真实 ZIP）。
    #[tokio::test]
    async fn mock_fetcher_download_copies_payload_file() {
        let dir = std::env::temp_dir().join(format!("ab-app-mock-payload-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let src = dir.join("payload.zip");
        std::fs::write(&src, b"zip-bytes").expect("write payload");
        let dest = dir.join("dest.zip");

        let fetcher = MockFetcher::default();
        fetcher.download_payload.lock().unwrap().replace(src);
        fetcher
            .download("https://example.com/payload.zip", &dest)
            .await
            .expect("download");
        assert_eq!(
            std::fs::read(&dest).expect("dest file"),
            b"zip-bytes",
            "注入 payload 时 dest 必须为 payload 内容副本"
        );

        let fetcher = MockFetcher::default();
        fetcher
            .download("https://example.com/empty.zip", &dest)
            .await
            .expect("download");
        assert_eq!(
            std::fs::read(&dest).expect("dest file"),
            b"",
            "未注入 payload 时仍落空文件（既有语义不变）"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 仅 https 可下载：http/file/不可解析形态一律拒绝。
    #[test]
    fn validate_download_url_requires_https() {
        for ok in [
            "https://github.com/o/r/releases/download/v1/a.zip",
            "HTTPS://github.com/o/r",
            "https://objects.githubusercontent.com/abc?token=x",
        ] {
            assert!(validate_download_url(ok).is_ok(), "should accept {ok:?}");
        }
        for bad in [
            "",
            "https://",
            "not a url",
            "http://github.com/o/r",
            "http://127.0.0.1:1/a.zip",
            "file:///tmp/a.zip",
            "ftp://host/a.zip",
        ] {
            assert!(
                matches!(validate_download_url(bad), Err(UpdateError::Network(_))),
                "should reject {bad:?}"
            );
        }
    }

    /// 伪造超大 Content-Length：预检直接拒绝（TooLarge），不做任何落盘。
    #[test]
    fn validate_download_response_rejects_oversized_content_length() {
        let headers = reqwest::header::HeaderMap::new();
        assert!(validate_download_response(
            &headers,
            "https://example.com/a.zip",
            None
        )
        .is_ok());
        assert!(validate_download_response(
            &headers,
            "https://example.com/a.zip",
            Some(MAX_ASSET_BYTES)
        )
        .is_ok());
        assert!(matches!(
            validate_download_response(
                &headers,
                "https://example.com/a.zip",
                Some(MAX_ASSET_BYTES + 1)
            ),
            Err(UpdateError::TooLarge)
        ));
    }

    /// 重定向后最终 URL 非 https（响应层预检）→ 拒绝。
    #[test]
    fn validate_download_response_rejects_non_https_final_url() {
        let headers = reqwest::header::HeaderMap::new();
        assert!(matches!(
            validate_download_response(&headers, "http://example.com/a.zip", None),
            Err(UpdateError::Network(_))
        ));
    }

    /// content-type 白名单：application/zip / application/octet-stream
    /// 允许（含参数段与大小写差异）；其他拒绝；缺失容忍。
    #[test]
    fn validate_download_response_allows_zip_octet_stream_and_missing() {
        for ct in [
            "application/zip",
            "application/octet-stream",
            "application/zip; charset=binary",
            "Application/Octet-Stream",
        ] {
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert(
                reqwest::header::CONTENT_TYPE,
                reqwest::header::HeaderValue::from_static(ct),
            );
            assert!(
                validate_download_response(&headers, "https://example.com/a.zip", None).is_ok(),
                "should accept content-type {ct:?}"
            );
        }
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/json"),
        );
        assert!(matches!(
            validate_download_response(&headers, "https://example.com/a.zip", None),
            Err(UpdateError::Network(_))
        ));
    }

    /// TooLarge 变体 Display 携带上限提示（用户可读）。
    #[test]
    fn too_large_display_mentions_limit() {
        let msg = UpdateError::TooLarge.to_string();
        assert!(msg.contains("500 MiB"), "message: {msg}");
    }

    /// 内存 chunk 队列（[`ChunkSource`] 测试实现；无需真实网络模拟 chunked 流）。
    struct MemoryChunkSource {
        chunks: VecDeque<Vec<u8>>,
        /// 前 n 块正常弹出后，下一次调用返回错误（注入流中断）。
        fail_after: Option<usize>,
    }

    impl MemoryChunkSource {
        fn new(chunks: Vec<Vec<u8>>) -> Self {
            Self {
                chunks: VecDeque::from(chunks),
                fail_after: None,
            }
        }

        fn failing_after(mut self, n: usize) -> Self {
            self.fail_after = Some(n);
            self
        }
    }

    #[async_trait::async_trait]
    impl ChunkSource for MemoryChunkSource {
        async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, UpdateError> {
            if self.fail_after == Some(0) {
                return Err(UpdateError::Network("injected stream failure".to_string()));
            }
            if let Some(n) = self.fail_after.as_mut() {
                *n -= 1;
            }
            Ok(self.chunks.pop_front())
        }
    }

    fn tmp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!("ab-app-bounded-{tag}-{}", std::process::id()))
    }

    /// 流式写入在限内：逐块落盘、返回实际写入字节数、文件内容完整。
    #[tokio::test]
    async fn bounded_download_writes_chunks_within_limit() {
        let dest = tmp_path("within");
        let _ = std::fs::remove_file(&dest);
        let dl = BoundedDownload::new(
            MemoryChunkSource::new(vec![
                b"hello".to_vec(),
                b" ".to_vec(),
                b"world".to_vec(),
            ]),
            &dest,
            1024,
        );
        let written = dl.run().await.expect("within limit");
        assert_eq!(written, 11);
        assert_eq!(std::fs::read(&dest).expect("read dest"), b"hello world");
        let _ = std::fs::remove_file(&dest);
    }

    /// chunked（无 Content-Length）流式超限：累计字节越过 limit 立即
    /// 中止并删除已写临时文件。
    #[tokio::test]
    async fn bounded_download_aborts_and_cleans_when_stream_exceeds_limit() {
        let dest = tmp_path("over");
        let _ = std::fs::remove_file(&dest);
        let dl = BoundedDownload::new(
            MemoryChunkSource::new(vec![vec![b'a'; 8], vec![b'b'; 8]]),
            &dest,
            10,
        );
        assert!(matches!(dl.run().await, Err(UpdateError::TooLarge)));
        assert!(!dest.exists(), "超限中止后必须删除已写临时文件");
    }

    /// 单块本身超过 limit：写前判定，同样中止并清理。
    #[tokio::test]
    async fn bounded_download_aborts_on_chunk_larger_than_limit() {
        let dest = tmp_path("hugechunk");
        let _ = std::fs::remove_file(&dest);
        let dl = BoundedDownload::new(
            MemoryChunkSource::new(vec![vec![b'x'; 16]]),
            &dest,
            8,
        );
        assert!(matches!(dl.run().await, Err(UpdateError::TooLarge)));
        assert!(!dest.exists(), "单块超限必须中止且无残留");
    }

    /// 流中断（源错误）：返回 Network 且清理已写部分文件。
    #[tokio::test]
    async fn bounded_download_cleans_partial_file_on_stream_failure() {
        let dest = tmp_path("fail");
        let _ = std::fs::remove_file(&dest);
        let src = MemoryChunkSource::new(vec![b"partial".to_vec()]).failing_after(1);
        let dl = BoundedDownload::new(src, &dest, 1024);
        assert!(matches!(dl.run().await, Err(UpdateError::Network(_))));
        assert!(!dest.exists(), "流失败后必须删除部分写入文件");
    }

    /// 目标不可写（父路径是文件 → create_dir_all 失败）：Network 错误且
    /// 不产生残留。
    #[tokio::test]
    async fn bounded_download_fails_when_dest_unwritable() {
        let dir = std::env::temp_dir().join(format!(
            "ab-app-bounded-parent-{}",
            std::process::id()
        ));
        let blocker = dir.join("blocker");
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(&blocker, b"x").expect("write blocker");
        let dest = blocker.join("child.bin");
        let dl = BoundedDownload::new(
            MemoryChunkSource::new(vec![b"data".to_vec()]),
            &dest,
            1024,
        );
        assert!(matches!(dl.run().await, Err(UpdateError::Network(_))));
        assert!(!dest.exists(), "创建失败不得留下文件");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 非 https URL 在发起任何请求前即被拒绝（端点不可达也能确定性
    /// 断言错误信息点名 https 要求）。
    #[tokio::test]
    async fn download_rejects_non_https_before_sending_request() {
        let fetcher = GitHubFetcher::new().expect("build client");
        let dest = tmp_path("scheme");
        let _ = std::fs::remove_file(&dest);
        let err = fetcher
            .download("http://127.0.0.1:1/a.zip", &dest)
            .await
            .expect_err("http scheme 必须拒绝");
        assert!(
            err.to_string().contains("https"),
            "错误信息应点名 https 要求：{err}"
        );
        assert!(!dest.exists(), "预检拒绝不得产生文件");
    }
}
