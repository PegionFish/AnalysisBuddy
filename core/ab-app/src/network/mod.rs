//! 更新检查网络抽象（模块管理器更新链路，任务 6 消费方）。
//!
//! [`UpdateFetcher`] 是所有更新逻辑的唯一网络入口（GitHub Releases 查询 /
//! 资产下载），测试优先：业务侧以 [`MockFetcher`] 驱动，生产侧以
//! [`GitHubFetcher`]（reqwest + rustls）实现。纯函数
//! [`parse_repo_url`] / [`tag_to_version`] / [`select_zip_asset`]
//! 与 [`MockFetcher`] 均为同步单元测试覆盖。

pub mod update_fetcher;

pub use update_fetcher::{
    parse_repo_url, select_zip_asset, tag_to_version, GitHubFetcher, MockFetcher, ReleaseAsset,
    ReleaseInfo, UpdateError, UpdateFetcher,
};
