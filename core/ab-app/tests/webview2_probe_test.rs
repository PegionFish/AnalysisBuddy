//! P4-01 WebView2 探测单测（ipc-ui.md §8.1 三态）：已安装 / 缺失 /
//! `pv = 0.0.0.0` 缺失占位值；另覆盖 HKLM 平铺与 HKCU 兜底顺序。
//!
//! 通过 `RegistryProbe` trait 注入假注册表读取器，不触碰真实注册表、
//! 不弹 MessageBox（引导框流程属手工验收项）。

use std::collections::HashMap;

use ab_app::webview2::{
    probe_webview2, RegistryHive, RegistryProbe, WebView2Status, EDGE_UPDATE_GUID,
};

/// 假注册表：hive + key 路径 → `pv` 值（None = 键或值不存在）。
struct FakeRegistry {
    values: HashMap<(RegistryHive, String), Option<String>>,
}

impl FakeRegistry {
    fn new() -> Self {
        Self {
            values: HashMap::new(),
        }
    }

    /// 写一个键：`Some(pv)` = 键存在且 pv 为给定值；`None` = 键存在但无 pv。
    fn set(&mut self, hive: RegistryHive, path: &str, pv: Option<String>) -> &mut Self {
        self.values.insert((hive, path.to_string()), pv);
        self
    }
}

impl RegistryProbe for FakeRegistry {
    fn read_pv(&self, hive: RegistryHive, key_path: &str) -> Option<String> {
        self.values.get(&(hive, key_path.to_string()))?.clone()
    }
}

fn wow6432() -> String {
    format!(r"SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{EDGE_UPDATE_GUID}")
}

fn hklm_plain() -> String {
    format!(r"SOFTWARE\Microsoft\EdgeUpdate\Clients\{EDGE_UPDATE_GUID}")
}

fn hkcu_plain() -> String {
    format!(r"SOFTWARE\Microsoft\EdgeUpdate\Clients\{EDGE_UPDATE_GUID}")
}

const REAL_VERSION: &str = "151.0.4129.72";
const MISSING_MARKER: &str = "0.0.0.0";

/// 已安装：WOW6432Node 首选键命中有效 pv。
#[test]
fn webview2_probe_installed_via_wow6432_node() {
    let mut reg = FakeRegistry::new();
    reg.set(
        RegistryHive::Hklm,
        &wow6432(),
        Some(REAL_VERSION.to_string()),
    );
    assert_eq!(
        probe_webview2(&reg),
        WebView2Status::Installed(REAL_VERSION.to_string())
    );
}

/// 已安装：32 位机/兜底——WOW6432Node 不存在，HKLM 平铺键命中。
#[test]
fn webview2_probe_installed_via_hklm_plain_fallback() {
    let mut reg = FakeRegistry::new();
    reg.set(
        RegistryHive::Hklm,
        &hklm_plain(),
        Some(REAL_VERSION.to_string()),
    );
    assert_eq!(
        probe_webview2(&reg),
        WebView2Status::Installed(REAL_VERSION.to_string())
    );
}

/// 已安装：仅用户级 HKCU 注册（HKLM 两键都缺）。
#[test]
fn webview2_probe_installed_via_hkcu_fallback() {
    let mut reg = FakeRegistry::new();
    reg.set(
        RegistryHive::HkcU,
        &hkcu_plain(),
        Some(REAL_VERSION.to_string()),
    );
    assert_eq!(
        probe_webview2(&reg),
        WebView2Status::Installed(REAL_VERSION.to_string())
    );
}

/// 缺失：三键全不存在。
#[test]
fn webview2_probe_missing_when_no_keys() {
    assert_eq!(
        probe_webview2(&FakeRegistry::new()),
        WebView2Status::Missing
    );
}

/// 缺失：键存在但 `pv` 为 `0.0.0.0` 占位值（视为缺失）。
#[test]
fn webview2_probe_missing_when_pv_is_zeroed() {
    let mut reg = FakeRegistry::new();
    reg.set(
        RegistryHive::Hklm,
        &wow6432(),
        Some(MISSING_MARKER.to_string()),
    );
    assert_eq!(probe_webview2(&reg), WebView2Status::Missing);
}

/// 缺失：键存在但无 pv 值。
#[test]
fn webview2_probe_missing_when_pv_absent() {
    let mut reg = FakeRegistry::new();
    reg.set(RegistryHive::Hklm, &wow6432(), None);
    assert_eq!(probe_webview2(&reg), WebView2Status::Missing);
}

/// 兜底语义：WOW6432Node 的 `0.0.0.0` 残留不遮蔽 HKCU 的有效注册
/// （跳过继续探测，与 §8.1「键存在且 pv 非 0.0.0.0 即视为已安装」一致）。
#[test]
fn webview2_probe_zeroed_wow6432_does_not_shadow_hkcu_valid() {
    let mut reg = FakeRegistry::new();
    reg.set(
        RegistryHive::Hklm,
        &wow6432(),
        Some(MISSING_MARKER.to_string()),
    );
    reg.set(
        RegistryHive::HkcU,
        &hkcu_plain(),
        Some(REAL_VERSION.to_string()),
    );
    assert_eq!(
        probe_webview2(&reg),
        WebView2Status::Installed(REAL_VERSION.to_string())
    );
}

/// 全空串 pv 同样视为缺失（防御空值残留）。
#[test]
fn webview2_probe_empty_pv_treated_as_missing() {
    let mut reg = FakeRegistry::new();
    reg.set(RegistryHive::Hklm, &wow6432(), Some(String::new()));
    assert_eq!(probe_webview2(&reg), WebView2Status::Missing);
}
