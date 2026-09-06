//! 引擎路径注入 seam（M1 步骤 4，Linux/服务器就绪）：引擎逻辑函数一律以
//! `&Path`/目录参数接收路径（不读环境变量）；宿主按自身平台公式构造
//! [`EnginePaths`] 后传入（smoke 装配与未来 ab-server 消费同一形状）。
//!
//! - 桌面壳（core/ab-app）：沿用既有 Windows 公式（exe 同目录 `plugins`、
//!   `%APPDATA%\AnalysisBuddy\{plugins,presets,sessions}`）构造，桌面行为
//!   与 M1 前逐值等价（原 `PluginRegistry::new()` / `presets_dir()` 公式）。
//! - 无头宿主（Linux）：[`EnginePaths::linux_default()`] 给出 XDG 约定默认
//!   （`$XDG_DATA_HOME` 或 `~/.local/share` + `/AnalysisBuddy/...`）。
//!
//! 本模块是 ab-engine 生产代码中唯一的环境变量读取点（`linux_default` 内）；
//! 引擎逻辑函数自身不读环境变量。

use std::ffi::OsStr;
use std::path::PathBuf;

/// 引擎全部文件系统路径的显式注入包（M1 步骤 4）。
///
/// 字段与发现三源对齐（`ab_host::PluginRegistry::with_sources` 同形）：
/// - `plugins_portable`：便携插件目录（exe 同目录 `plugins`；模块状态文件
///   `.ab-modules.json` 亦落此目录，spec §3.2）。
/// - `plugins_install`：InstallDir 源（ZIP 布局下与 Portable 同路径）。
/// - `plugins_user`：用户数据插件目录（Windows `%APPDATA%` / Linux XDG）。
/// - `presets_dir`：用户预设目录（`<id>.abpreset.json`）。
/// - `sessions_dir`：宿主侧会话/数据预留目录（M1 无消费方，为 ab-server
///   预留；命名对齐 `*.absession` 落盘约定）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnginePaths {
    pub plugins_portable: PathBuf,
    pub plugins_install: PathBuf,
    pub plugins_user: PathBuf,
    pub presets_dir: PathBuf,
    pub sessions_dir: PathBuf,
}

impl EnginePaths {
    /// Linux 默认路径（XDG Base Directory）：数据根 = `$XDG_DATA_HOME`
    /// （非空且为绝对路径时生效，否则按规范忽略）→ 回落
    /// `$HOME/.local/share`；其下 `AnalysisBuddy/{plugins,presets,sessions}`。
    pub fn linux_default() -> Self {
        Self::from_data_dir(linux_data_dir())
    }

    /// 由数据根目录派生完整布局（`<data>/AnalysisBuddy/...`）。
    fn from_data_dir(data_dir: PathBuf) -> Self {
        let base = data_dir.join("AnalysisBuddy");
        Self {
            plugins_portable: base.join("plugins"),
            plugins_install: base.join("plugins"),
            plugins_user: base.join("plugins"),
            presets_dir: base.join("presets"),
            sessions_dir: base.join("sessions"),
        }
    }
}

/// 当前环境下的 Linux 数据根（XDG 读取点）：`$XDG_DATA_HOME`（绝对路径时
/// 生效）或 `$HOME/.local/share`。无 `$HOME` 时回落空路径组件（调用方注入
/// 显式路径即可覆盖，不 panic）。
fn linux_data_dir() -> PathBuf {
    linux_data_dir_from(
        std::env::var_os("XDG_DATA_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

/// 纯函数核心（测试注入参数，不触环境）：XDG Base Directory 规范——
/// `$XDG_DATA_HOME` 非绝对路径必须忽略，回落 `$HOME/.local/share`。
fn linux_data_dir_from(xdg_data_home: Option<&OsStr>, home: Option<&OsStr>) -> PathBuf {
    if let Some(dir) = xdg_data_home.map(PathBuf::from).filter(|p| p.is_absolute()) {
        return dir;
    }
    home.map(PathBuf::from)
        .unwrap_or_default()
        .join(".local")
        .join("share")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 平台无关的绝对路径（Windows 的 `is_absolute` 要求盘符前缀，
    /// Unix 风格 `/x` 在 Windows 上非绝对；`temp_dir()` 两平台均绝对）。
    fn abs(name: &str) -> PathBuf {
        std::env::temp_dir().join(name)
    }

    #[test]
    fn xdg_absolute_path_wins() {
        let xdg = abs("ab-engine-xdg");
        let dir = linux_data_dir_from(Some(xdg.as_os_str()), Some(OsStr::new("anyhome")));
        assert_eq!(dir, xdg);
    }

    #[test]
    fn xdg_relative_or_missing_falls_back_to_home() {
        let home = abs("ab-engine-home");
        let relative = linux_data_dir_from(Some(OsStr::new("relative")), Some(home.as_os_str()));
        assert_eq!(relative, home.join(".local").join("share"));
        let missing = linux_data_dir_from(None, Some(home.as_os_str()));
        assert_eq!(missing, home.join(".local").join("share"));
    }

    #[test]
    fn linux_default_layout_under_analysisbuddy() {
        let root = PathBuf::from("data-root");
        let paths = EnginePaths::from_data_dir(root.clone());
        let base = root.join("AnalysisBuddy");
        assert_eq!(paths.plugins_portable, base.join("plugins"));
        assert_eq!(paths.plugins_user, base.join("plugins"));
        assert_eq!(paths.presets_dir, base.join("presets"));
        assert_eq!(paths.sessions_dir, base.join("sessions"));
    }
}
