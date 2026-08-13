//! WebView2 运行时缺失检测与引导（ipc-ui.md §8.1）+ 渲染器无障碍
//! 浏览器参数装配（e2e-uiux-report §6 A11y）。
//!
//! 交付形态是纯 ZIP（无安装器、无 Evergreen bootstrapper，§8.2），故在
//! Rust 侧 main 早期、建 WebView 窗口之前做只读注册表探测：
//!
//! 1. `HKLM\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{GUID}`（64 位机首选）
//! 2. `HKLM\SOFTWARE\Microsoft\EdgeUpdate\Clients\{GUID}`（32 位机/兜底）
//! 3. `HKCU\SOFTWARE\Microsoft\EdgeUpdate\Clients\{GUID}`（用户级兜底）
//!
//! 键存在且 `pv` 非 `0.0.0.0`（非空）即视为已安装；`pv = 0.0.0.0` 视为缺失
//! （跳过继续探测后续键）。整个流程只读，无任何注册表写入、无安装器行为。
//!
//! 缺失时不创建窗口，改弹原生 `MessageBox`（中英双语文案）+「打开下载页」
//! `ShellExecute` 到 Evergreen 下载页 /「退出」。`RegistryProbe` trait 抽象
//! 注册表读取器，单测注入假实现覆盖三态（见 `tests/webview2_probe_test.rs`）。

use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
use winreg::RegKey;

use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    MessageBoxW, IDNO, MB_APPLMODAL, MB_DEFBUTTON1, MB_ICONERROR, MB_SETFOREGROUND, MB_TOPMOST,
    MB_YESNO,
};

/// Evergreen 运行时注册表客户端 GUID（x64/ARM64 共用同一 Evergreen 运行时）。
pub const EDGE_UPDATE_GUID: &str = "{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";

/// Microsoft Edge WebView2 官方下载页（Evergreen Standalone Installer 指引）。
pub const WEBVIEW2_DOWNLOAD_URL: &str = "https://developer.microsoft.com/microsoft-edge/webview2/";

/// WebView2 无障碍：强制渲染器常驻构建 AX 树（e2e-uiux-report-2026-08-13.md §6）。
///
/// 背景：WebView2 内容位于 `WRY_WEBVIEW → Chrome_RenderWidgetHostHWND`，Chromium 默认
/// 只在检测到辅助技术客户端时才构建 UIA 可访问树，`get_window_state`/读屏器因此只见
/// 标题栏。`--force-renderer-accessibility` 令渲染器无条件构建 AX 树，使按钮/输入/
/// 图表等 DOM 元素对 UIA/AX、键盘导航与自动化工具可见。
///
/// 副作用：常驻 AX 树会增加少量渲染内存与 CPU（图表帧率回归风险见 dev-todo 计划 §7），
/// 属「以可访问性换取可量化的微小开销」，符合 §6 建议。
pub const A11Y_FORCE_RENDERER_ACCESSIBILITY: &str = "--force-renderer-accessibility";

/// wry 0.55 在未显式设置时注入的默认浏览器参数（wry `webview2/mod.rs` `create_environment`）：
/// 移除 Edge「mini menu」与 SmartScreen 提示。注意：一旦显式设置 `additionalBrowserArgs`，
/// wry 会整体替换默认串（见 wry 源码 `unwrap_or_else` 分支），因此完整参数串必须
/// **自行带上前缀**，否则会回退开启 `msWebOOUI/msPdfOOUI/msSmartScreenProtection`。
pub const WRY_DEFAULT_BROWSER_ARGS: &str =
    "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection";

/// 装配发送给 WebView2 的完整 `additionalBrowserArgs`（tauri.conf.json
/// `app.windows[].additionalBrowserArgs` 的取值）。
///
/// Tauri 2 的传递链：`WindowConfig.additional_browser_args` → `WebviewAttributes` →
/// wry `WebViewBuilder::with_additional_browser_args` →
/// `CoreWebView2EnvironmentOptions::set_additional_browser_arguments`
/// （tauri-runtime-wry `lib.rs`，已核对 tauri 2.11.5 / wry 0.55.1 源码）。
///
/// 注意：wry 对 `additionalBrowserArguments` **无条件**调用
/// `set_additional_browser_arguments`，因此 `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS`
/// 环境变量会被覆盖、不起作用（e2e 报告 §7.5 已实测 CDP 参数未生效），
/// 必须走窗口级 `additionalBrowserArgs` 配置。
///
/// 返回值形如：
/// `--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection --force-renderer-accessibility`
pub fn webview2_a11y_browser_args() -> String {
    format!("{WRY_DEFAULT_BROWSER_ARGS} {A11Y_FORCE_RENDERER_ACCESSIBILITY}")
}

/// 视为「缺失」的占位版本值（运行时未正确安装时的残留注册值）。
const PV_MISSING_MARKER: &str = "0.0.0.0";

/// 探测顺序（§8.1 第 1 条）：WOW6432Node → HKLM 平铺 → HKCU 兜底。
const PROBE_KEYS: [(RegistryHive, &str); 3] = [
    (
        RegistryHive::Hklm,
        r"SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients",
    ),
    (RegistryHive::Hklm, r"SOFTWARE\Microsoft\EdgeUpdate\Clients"),
    (RegistryHive::HkcU, r"SOFTWARE\Microsoft\EdgeUpdate\Clients"),
];

/// 可注入的注册表读取器（单测以假实现替换，见 `tests/webview2_probe_test.rs`）。
pub trait RegistryProbe {
    /// 读 `hive` 下 `key_path` 键的 `pv` 值；键或值不存在返回 `None`。
    fn read_pv(&self, hive: RegistryHive, key_path: &str) -> Option<String>;
}

/// 注册表根键（探测顺序的输入）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RegistryHive {
    Hklm,
    HkcU,
}

/// 真实注册表实现（只读，§8.2：无写入）。
pub struct WinRegistry;

impl RegistryProbe for WinRegistry {
    fn read_pv(&self, hive: RegistryHive, key_path: &str) -> Option<String> {
        let root = match hive {
            RegistryHive::Hklm => HKEY_LOCAL_MACHINE,
            RegistryHive::HkcU => HKEY_CURRENT_USER,
        };
        let key = RegKey::predef(root).open_subkey(key_path).ok()?;
        key.get_value("pv").ok()
    }
}

/// 探测结果三态。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WebView2Status {
    /// 已安装，携带 `pv` 版本号。
    Installed(String),
    /// 未安装或 `pv` 为缺失占位值。
    Missing,
}

/// 缺失引导框的用户选择。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GuideAction {
    /// 「打开下载页」→ `ShellExecute` 到 Evergreen 下载页后退出。
    OpenDownload,
    /// 「退出」。
    Exit,
}

/// 按 §8.1 顺序探测：任一键返回有效 `pv`（非空、非 `0.0.0.0`）即已安装；
/// `pv = 0.0.0.0` 跳过继续探测（残留值不遮蔽用户级有效注册）。
pub fn probe_webview2<P: RegistryProbe>(probe: &P) -> WebView2Status {
    for (hive, base_path) in PROBE_KEYS {
        let key_path = format!(r"{base_path}\{EDGE_UPDATE_GUID}");
        let pv = probe.read_pv(hive, &key_path);
        if let Some(pv) = pv {
            let pv = pv.trim();
            if !pv.is_empty() && pv != PV_MISSING_MARKER {
                return WebView2Status::Installed(pv.to_string());
            }
        }
    }
    WebView2Status::Missing
}

/// 缺失流程（§8.1 第 2 条）：原生 MessageBox（中英双语）+ 下载页/退出。
/// 按钮语义：是(Y) = 打开下载页；否(N) = 退出。
pub fn guide_user_missing_webview2() -> GuideAction {
    let message = concat!(
        "无法启动：未检测到 Microsoft Edge WebView2 运行时。\n",
        "请点击「打开下载页」安装 Evergreen Standalone Installer，安装完成后重新启动本程序。\n\n",
        "Cannot start: Microsoft Edge WebView2 Runtime was not detected.\n",
        "Click \"Open download page\" to install the Evergreen Standalone Installer, ",
        "then restart the application."
    );
    let title = "AnalysisBuddy — WebView2 运行时缺失 / WebView2 Runtime Missing";
    // 按钮文案由系统本地化：是(Y) → 打开下载页；否(N) → 退出。
    let choice = unsafe {
        MessageBoxW(
            std::ptr::null_mut(), // HWND(0)：无父窗口，系统级引导框
            to_wide(message).as_ptr(),
            to_wide(title).as_ptr(),
            MB_YESNO | MB_ICONERROR | MB_DEFBUTTON1 | MB_APPLMODAL | MB_SETFOREGROUND | MB_TOPMOST,
        )
    };
    if choice == IDNO {
        GuideAction::Exit
    } else {
        // IDYES / 关闭窗口（默认）一律视为「打开下载页」。
        GuideAction::OpenDownload
    }
}

/// 打开 Evergreen 官方下载页（`ShellExecute`，§8.1 第 2 条）。返回是否发起成功。
pub fn open_webview2_download_page() -> bool {
    let url = to_wide(WEBVIEW2_DOWNLOAD_URL);
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(), // HWND(0)
            to_wide("open").as_ptr(),
            url.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1, // SW_SHOWNORMAL
        )
    };
    // ShellExecute 成功时返回值 > 32。
    result as isize > 32
}

/// 一键流程（main 早期建窗前调用，ipc-ui.md §8.1）：
/// 已安装 → `true`（继续正常建窗启动）；缺失 → 弹引导框，按用户选择
/// 打开下载页或直接退出，返回 `false`（调用方不得再创建窗口）。
///
/// 仅在 release（生产）路径生效：`debug_assertions` 下跳过探测，保证
/// `cargo tauri dev` 开发流程不受门禁影响。
pub fn ensure_webview2() -> bool {
    if cfg!(debug_assertions) {
        return true;
    }
    match probe_webview2(&WinRegistry) {
        WebView2Status::Installed(_) => true,
        WebView2Status::Missing => {
            if guide_user_missing_webview2() == GuideAction::OpenDownload {
                open_webview2_download_page();
            }
            false
        }
    }
}

/// UTF-16 宽字符缓存（Windows API 入参；跨 FFI 后不再持有）。
fn to_wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// §6 A11y：参数串必须包含强制 AX 树开关（读屏器/UIA 可见主体的关键）。
    #[test]
    fn a11y_args_force_renderer_accessibility() {
        let args = webview2_a11y_browser_args();
        assert!(
            args.contains(A11Y_FORCE_RENDERER_ACCESSIBILITY),
            "缺少 --force-renderer-accessibility：{args}"
        );
    }

    /// §6 A11y：显式设置 additionalBrowserArgs 会整体替换 wry 默认串，
    /// 故必须自行保留默认 disable-features 前缀，避免回退开启 OOUI/SmartScreen。
    #[test]
    fn a11y_args_preserve_wry_default_prefix() {
        let args = webview2_a11y_browser_args();
        assert!(
            args.starts_with(WRY_DEFAULT_BROWSER_ARGS),
            "必须以 wry 默认串开头：{args}"
        );
    }

    /// 装配稳定：默认串 + 空格 + 开关，顺序固定，便于 tauri.conf.json 直接照抄。
    #[test]
    fn a11y_args_exact_assembly() {
        assert_eq!(
            webview2_a11y_browser_args(),
            format!("{WRY_DEFAULT_BROWSER_ARGS} {A11Y_FORCE_RENDERER_ACCESSIBILITY}")
        );
        // 不含尾随空格/换行，避免向 WebView2 传递空参数段。
        assert_eq!(
            webview2_a11y_browser_args().trim(),
            webview2_a11y_browser_args()
        );
    }
}
