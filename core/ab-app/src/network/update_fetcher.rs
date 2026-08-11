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
    /// 网络层失败（连接 / 传输 / 本地 IO / 超大资产拒绝等）。
    Network(String),
    /// 未恰好命中一个 zip 资产；载荷为 zip 资产个数。
    NoZipAsset(usize),
    /// GitHub API 非 2xx；载荷为状态码文本。
    Api(String),
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UpdateError::RepoParse => write!(f, "invalid repository reference"),
            UpdateError::Network(msg) => write!(f, "network error: {msg}"),
            UpdateError::NoZipAsset(n) => write!(f, "expected exactly one .zip asset, found {n}"),
            UpdateError::Api(msg) => write!(f, "GitHub API error: {msg}"),
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

/// 解析仓库引用：接受 `https://github.com/o/r` 与裸 `o/r`，拒绝其余形态。
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

/// GitHub Releases 生产实现：reqwest + rustls，30s 请求超时。
#[derive(Debug, Clone)]
pub struct GitHubFetcher {
    client: reqwest::Client,
}

impl GitHubFetcher {
    /// 构造客户端（30s 无数据超时：每次成功读取后重置）。
    pub fn new() -> Result<Self, UpdateError> {
        let client = reqwest::Client::builder()
            .read_timeout(Duration::from_secs(30))
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
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| UpdateError::Network(e.to_string()))?;
        if !response.status().is_success() {
            return Err(UpdateError::Api(response.status().to_string()));
        }
        if let Some(len) = response.content_length() {
            if len > MAX_ASSET_BYTES {
                return Err(UpdateError::Network(format!(
                    "asset too large ({len} bytes, limit 500 MiB)"
                )));
            }
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).map_err(|e| UpdateError::Network(e.to_string()))?;
        }
        // workspace tokio 未启用 `fs` feature，此处同步写文件；分块流式落盘，
        // 不整包驻留内存。
        let mut file =
            std::fs::File::create(dest).map_err(|e| UpdateError::Network(e.to_string()))?;
        let mut response = response;
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| UpdateError::Network(e.to_string()))?
        {
            file.write_all(&chunk)
                .map_err(|e| UpdateError::Network(e.to_string()))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// 其余形态一律拒绝：缺 owner/repo、多段路径、非 https、非 github.com。
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
}
