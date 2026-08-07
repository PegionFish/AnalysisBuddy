//! 插件子进程拉起（host-runtime.md §3.1；protocol.md §7.3 宿主拉起规则）。

use std::process::Stdio;

use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout};

use crate::manifest::ResolvedEntry;
use crate::HostError;

/// Windows：避免插件控制台窗口闪现（§3.1）。
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 拉起的子进程及其三条管道（§1.1 独占协议通道 + stderr 日志通道）。
pub struct SpawnedChild {
    pub child: Child,
    pub stdin: ChildStdin,
    pub stdout: ChildStdout,
    pub stderr: ChildStderr,
}

/// 进程拉起器。只负责 Command 组装与 spawn，启动后的失败检测在会话层。
#[derive(Debug, Default, Clone, Copy)]
pub struct PluginSpawner;

impl PluginSpawner {
    /// 按解析后的入口组装启动命令（§3.1）：
    /// `working_dir` 为进程工作目录；Windows 下带 `CREATE_NO_WINDOW`；
    /// 三管道全部 piped；环境变量继承宿主（解释器型入口依赖 PATH）。
    ///
    /// 注：入参为 [`ResolvedEntry`]（`resolve_entry` 的产物），因为 spawn 需要
    /// 解析后的 program/working_dir；manifest 自身不携带这些信息。
    pub fn spawn(&self, entry: &ResolvedEntry) -> Result<SpawnedChild, HostError> {
        let mut cmd = tokio::process::Command::new(&entry.program);
        cmd.args(&entry.args)
            .current_dir(&entry.working_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        cmd.creation_flags(CREATE_NO_WINDOW);
        let mut child = cmd
            .spawn()
            .map_err(|e| HostError::Transport(format!("spawn plugin process: {e}")))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| HostError::Transport("plugin stdin pipe unavailable".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| HostError::Transport("plugin stdout pipe unavailable".to_string()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| HostError::Transport("plugin stderr pipe unavailable".to_string()))?;
        Ok(SpawnedChild {
            child,
            stdin,
            stdout,
            stderr,
        })
    }
}
