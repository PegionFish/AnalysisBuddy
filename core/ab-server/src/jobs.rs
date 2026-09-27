//! 导入任务（Job）注册表：POST /imports 把导入包装为后台任务（202 +
//! job_id），状态机 queued → running → completed/failed/cancelled。
//!
//! v1 取消语义（协作式，文档化于 http-api-v1.md §2）：管线事件
//! `ImportStarted` 只带 path 不带 file_id，无法即时中断 in-flight 单文件
//! 导入——DELETE /imports/{job_id} 置取消旗标：queued 任务就地翻转
//! Cancelled；running 任务在下一个文件边界停止（剩余路径不再开始，已完成
//! 文件结果保留，终态 Cancelled）。
//!
//! 并发闸：`tokio::sync::Semaphore`（`--max-concurrent-imports`，默认 2）
//! 在真正调用 `import_files_logic` 前获取 permit——排队上限不受闸约束，
//! 执行并发受限。全部锁为 std `Mutex`（只在查询/写入瞬间持有，不跨 await）。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

// C6（卷三主题 4）：parking_lot 无锁毒化——持有锁 panic 不再永久毒化
// 该资源（std Mutex/RwLock 的 poison → 之后所有请求永久失败）。
use parking_lot::{Mutex, RwLock};

use ab_engine::commands::import::import_files_logic;
use ab_engine::commands::{ImportOverride, ImportResultDto, IpcError};
use ab_engine::pipeline_bridge::ImportCoordinator;
use serde::Serialize;
use tokio::sync::Semaphore;

/// 任务状态（serde snake_case：queued/running/completed/failed/cancelled）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

/// GET /imports/{job_id} 响应体（粗状态 + 终态文件结果；进行中时
/// files/error 键省略）。
#[derive(Debug, Clone, Serialize)]
pub struct JobStatusDto {
    pub job_id: String,
    pub state: JobState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<IpcError>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<ImportResultDto>,
}

/// 注册表内部任务记录。
#[derive(Debug)]
struct Job {
    state: JobState,
    files: Vec<ImportResultDto>,
    error: Option<IpcError>,
}

/// 任务注册表 + 并发闸（spawn 后一切在 `run_import` 内推进）。
pub struct JobRegistry {
    next_id: AtomicU64,
    semaphore: Arc<Semaphore>,
    jobs: Mutex<HashMap<String, Job>>,
    cancel_flags: Mutex<HashMap<String, Arc<AtomicBool>>>,
    /// C9：job_id → 在途文件路径（run_import 每 per-file 窗口登记/清除），
    /// 取消时定位引擎侧 cancel_parse 目标。
    inflight_paths: Mutex<HashMap<String, String>>,
}

impl JobRegistry {
    pub fn new(max_concurrent_imports: usize) -> Self {
        Self {
            next_id: AtomicU64::new(1),
            semaphore: Arc::new(Semaphore::new(max_concurrent_imports.max(1))),
            jobs: Mutex::new(HashMap::new()),
            cancel_flags: Mutex::new(HashMap::new()),
            inflight_paths: Mutex::new(HashMap::new()),
        }
    }

    /// 登记并 spawn 一个导入任务，返回 queued 快照（202 响应体）。
    ///
    /// `cleanup_paths`（WS-B2/P0-4，清理契约强制项）：任务进入终态
    /// （completed/failed/cancelled——含排队期取消）时 best-effort 删除的
    /// 上传副本路径；路径导入形态传空（桌面本地文件不属于服务端所有权，
    /// 不得删除）。删除失败只记 stderr，不改变任务终态语义。
    pub fn spawn_import(
        self: &Arc<Self>,
        coordinator: Arc<ImportCoordinator>,
        paths: Vec<String>,
        overrides: Option<HashMap<String, ImportOverride>>,
        cleanup_paths: Vec<PathBuf>,
    ) -> JobStatusDto {
        let job_id = format!("job-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        self.jobs.lock().insert(
            job_id.clone(),
            Job {
                state: JobState::Queued,
                files: Vec::new(),
                error: None,
            },
        );
        let flag = Arc::new(AtomicBool::new(false));
        self.cancel_flags
            .lock()
            .insert(job_id.clone(), flag.clone());
        let registry = Arc::clone(self);
        let job_for_task = job_id.clone();
        tokio::spawn(async move {
            registry
                .run_import(
                    job_for_task,
                    coordinator,
                    paths,
                    overrides,
                    flag,
                    cleanup_paths,
                )
                .await;
        });
        self.status(&job_id).expect("job just inserted")
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_import(
        self: Arc<Self>,
        job_id: String,
        coordinator: Arc<ImportCoordinator>,
        paths: Vec<String>,
        overrides: Option<HashMap<String, ImportOverride>>,
        flag: Arc<AtomicBool>,
        cleanup_paths: Vec<PathBuf>,
    ) {
        let mut files: Vec<ImportResultDto> = Vec::new();
        let mut error: Option<IpcError> = None;
        // 排队等并发闸（queued 期间被取消则直接终态，permit 自动释放）。
        let permit = self.semaphore.clone().acquire_owned().await;
        if flag.load(Ordering::SeqCst) {
            drop(permit);
            cleanup_upload_copies(&cleanup_paths);
            self.finish(&job_id, JobState::Cancelled, files, None);
            return;
        }
        let Ok(permit) = permit else {
            cleanup_upload_copies(&cleanup_paths);
            return; // semaphore 关闭：不发生（registry 存活期间不 drop）
        };
        self.mark_running(&job_id);

        let mut stopped = false;
        for path in paths {
            if flag.load(Ordering::SeqCst) {
                stopped = true;
                break;
            }
            // C9：登记在途路径供取消路径即时接线 cancel_parse。
            self.inflight_paths
                .lock()
                .insert(job_id.clone(), path.clone());
            // per-path overrides（与桌面一致：按路径键查找手选覆盖）。
            let own_overrides = overrides
                .as_ref()
                .and_then(|map| map.get(&path).cloned())
                .map(|entry| HashMap::from([(path.clone(), entry)]));
            let result = import_files_logic(&coordinator, vec![path], own_overrides).await;
            self.inflight_paths
                .lock()
                .remove(&job_id);
            match result {
                Ok(mut results) => files.append(&mut results),
                Err(e) => {
                    error = Some(e);
                    break;
                }
            }
        }
        drop(permit);

        // 失败优先于取消（真实错误信息更有价值）；旗标兜底覆盖检查点窗口。
        let state = if error.is_some() {
            JobState::Failed
        } else if stopped || flag.load(Ordering::SeqCst) {
            JobState::Cancelled
        } else {
            JobState::Completed
        };
        // WS-B4：needs_user_choice 的上传副本保留——手选插件后的重试导入仍
        // 需引用该副本；残留由会话终结兜底（网关 teardown rm -rf 实例 TMPDIR，
        // 清理契约不变式 I-1 不受影响）。其余终态照常即删（WS-B2/P0-4）。
        if files.iter().any(|f| f.needs_user_choice == Some(true)) {
            eprintln!(
                "ab-server: upload copy kept for user plugin choice ({} path(s))",
                cleanup_paths.len()
            );
        } else {
            cleanup_upload_copies(&cleanup_paths);
        }
        self.finish(&job_id, state, files, error);
    }

    /// 取消任务：置旗标（running 任务在文件边界停止；引擎侧 cancel_parse
    /// 由路由层经 [`Self::take_inflight_path`] 接线即时中断）；queued 任务
    /// 就地翻转 Cancelled。**终态任务不再 404**（卷三 A1#3：旗标已随 finish
    /// 清理，但契约要求返回终态快照）；仅未知 job_id → None（404）。
    pub fn cancel(&self, job_id: &str) -> Option<JobStatusDto> {
        {
            // Arc<AtomicBool> 经共享引用即可 store（无需 mut 绑定）。
            let flags = self.cancel_flags.lock();
            if let Some(flag) = flags.get(job_id) {
                flag.store(true, Ordering::SeqCst);
            }
            // 无旗标（终态）→ 只回落快照，不 404
        }
        let mut jobs = self.jobs.lock();
        let job = jobs.get_mut(job_id)?;
        if job.state == JobState::Queued {
            job.state = JobState::Cancelled;
        }
        Some(snapshot_of(job_id, job))
    }

    /// C9：取出并清除该 job 的在途文件路径（存在 = 取消时刻正在解析）。
    pub fn take_inflight_path(&self, job_id: &str) -> Option<String> {
        self.inflight_paths
            .lock()
            
            .remove(job_id)
    }

    /// 当前任务状态（未知 job_id → None）。
    pub fn status(&self, job_id: &str) -> Option<JobStatusDto> {
        let jobs = self.jobs.lock();
        let job = jobs.get(job_id)?;
        Some(snapshot_of(job_id, job))
    }

    fn mark_running(&self, job_id: &str) {
        let mut jobs = self.jobs.lock();
        if let Some(job) = jobs.get_mut(job_id) {
            if job.state == JobState::Queued {
                job.state = JobState::Running;
            }
        }
    }

    /// 写入终态（cancel 与 finish 竞争时 Cancelled 优先——任务已对客户端
    /// 呈现取消）；随后清理取消旗标（已完成任务不可再取消）。
    fn finish(
        &self,
        job_id: &str,
        state: JobState,
        files: Vec<ImportResultDto>,
        error: Option<IpcError>,
    ) {
        {
            let mut jobs = self.jobs.lock();
            if let Some(job) = jobs.get_mut(job_id) {
                if job.state != JobState::Cancelled || state == JobState::Cancelled {
                    job.state = state;
                    job.files = files;
                    job.error = error;
                }
            }
        }
        self.cancel_flags.lock().remove(job_id);
    }
}

fn snapshot_of(job_id: &str, job: &Job) -> JobStatusDto {
    JobStatusDto {
        job_id: job_id.to_string(),
        state: job.state,
        error: job.error.clone(),
        files: job.files.clone(),
    }
}

// ---------------------------------------------------------------------------
// 上传副本清理（WS-B2 / P0-4，卷一 §1.3 清理矩阵强制项）
// ---------------------------------------------------------------------------

/// best-effort 删除本任务拥有的上传副本（`<TMPDIR>/ab-server-uploads/<pid>-
/// <seq>-<nanos>/<basename>`）：job 终态即删，不等会话终结。删除失败只记
/// stderr（不改变任务终态语义）；副本的父目录（本任务专属）在文件删净后
/// 一并移除。
pub(crate) fn cleanup_upload_copies(paths: &[PathBuf]) {
    for path in paths {
        if let Err(e) = std::fs::remove_file(path) {
            if e.kind() != std::io::ErrorKind::NotFound {
                eprintln!(
                    "ab-server: upload copy cleanup failed for {}: {e}",
                    path.display()
                );
            }
            continue;
        }
        if let Some(dir) = path.parent() {
            let _ = std::fs::remove_dir(dir); // 目录非空（不应发生）则保留，无害
        }
    }
}

/// 启动清扫：`<TMPDIR>/ab-server-uploads/` 下**其他已死进程**的历史副本
/// 目录整目录移除。同 pid（本进程此前实例不可能存在；同进程内并发 assemble
/// 场景——如测试——视为存活）与其他存活进程的目录跳过，杜绝误删并发实例
/// 的在途副本。网关形态下每实例 TMPDIR 独立，启动时该根内一切皆本实例
/// 残留（同 pid 分支仅桌面形态可达）。
pub(crate) fn sweep_stale_uploads() {
    let root = std::env::temp_dir().join("ab-server-uploads");
    let Ok(entries) = std::fs::read_dir(&root) else {
        return;
    };
    let our_pid = std::process::id();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(pid) = name.split('-').next().and_then(|p| p.parse::<u32>().ok()) else {
            continue; // 命名不符（非 <pid>- 前缀）：不动
        };
        if pid == our_pid || pid_is_alive(pid) {
            continue;
        }
        if let Err(e) = std::fs::remove_dir_all(entry.path()) {
            if e.kind() != std::io::ErrorKind::NotFound {
                eprintln!(
                    "ab-server: stale upload sweep failed for {}: {e}",
                    entry.path().display()
                );
            }
        }
    }
}

#[cfg(unix)]
fn pid_is_alive(pid: u32) -> bool {
    // ps -p <pid>：退出码 0 = 存在。探测失败按存活处理（保守，不误删）。
    std::process::Command::new("ps")
        .arg("-p")
        .arg(pid.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(true)
}

#[cfg(windows)]
fn pid_is_alive(pid: u32) -> bool {
    // tasklist 过滤无命中时退出码仍为 0（打印 INFO 行）——此探测在 Windows
    // 上偏保守（死进程可能被当活 → 跳过清扫、残留无害，绝不误删）。
    std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}")])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manual_job(registry: &JobRegistry, job_id: &str) {
        registry.jobs.lock().insert(
            job_id.to_string(),
            Job {
                state: JobState::Queued,
                files: Vec::new(),
                error: None,
            },
        );
        registry
            .cancel_flags
            .lock()
            .insert(job_id.to_string(), Arc::new(AtomicBool::new(false)));
    }

    #[test]
    fn job_state_serde_is_snake_case() {
        assert_eq!(serde_json::to_value(JobState::Queued).unwrap(), "queued");
        assert_eq!(serde_json::to_value(JobState::Running).unwrap(), "running");
        assert_eq!(
            serde_json::to_value(JobState::Completed).unwrap(),
            "completed"
        );
        assert_eq!(serde_json::to_value(JobState::Failed).unwrap(), "failed");
        assert_eq!(
            serde_json::to_value(JobState::Cancelled).unwrap(),
            "cancelled"
        );
    }

    #[test]
    fn cancel_queued_flips_in_place_and_unknown_is_none() {
        let registry = JobRegistry::new(1);
        manual_job(&registry, "job-7");
        let status = registry.cancel("job-7").expect("cancel");
        assert_eq!(status.state, JobState::Cancelled);
        // 旗标在任务收尾（run_import → finish）时清理；终态后取消按契约
        // 返回终态快照（C9，不再 404），仅未知 job_id → None。
        registry.finish("job-7", JobState::Cancelled, Vec::new(), None);
        let snap = registry.cancel("job-7").expect("终态取消返回快照");
        assert_eq!(snap.state, JobState::Cancelled);
        assert!(registry.cancel("job-nope").is_none());
    }

    #[test]
    fn finish_preserves_cancelled_over_completion() {
        let registry = JobRegistry::new(1);
        manual_job(&registry, "job-8");
        registry.cancel("job-8");
        // run_import 迟到完成：终态保持 Cancelled。
        registry.finish("job-8", JobState::Completed, Vec::new(), None);
        assert_eq!(
            registry.status("job-8").expect("status").state,
            JobState::Cancelled
        );
    }

    /// WS-B2：副本清理——文件与专属父目录一并删除；不存在路径静默通过。
    #[test]
    fn cleanup_upload_copies_removes_file_and_dir() {
        let base = std::env::temp_dir().join(format!(
            "ab-jobs-cleanup-{}-{}",
            std::process::id(),
            line!()
        ));
        let dir = base.join("100-1-1");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.csv");
        std::fs::write(&file, b"x").unwrap();
        cleanup_upload_copies(&[file.clone(), base.join("nope-2-2/missing.bin")]);
        assert!(!file.exists());
        assert!(!dir.exists(), "专属父目录随文件移除");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// WS-B2：启动清扫——死 pid 目录移除；本 pid 与存活 pid（取 init=1）
    /// 目录保留；非 <pid>- 前缀目录不动。
    #[test]
    fn sweep_removes_only_dead_pid_dirs() {
        let root = std::env::temp_dir().join("ab-server-uploads");
        std::fs::create_dir_all(&root).unwrap();
        let stamp = format!("{}-{}", std::process::id(), line!());
        let dead = root.join(format!("999999999-{stamp}"));
        let ours = root.join(format!("{}-{stamp}", std::process::id()));
        let live = root.join(format!("1-{stamp}"));
        let alien = root.join("not-a-pid-dir");
        for d in [&dead, &ours, &live, &alien] {
            std::fs::create_dir_all(d).unwrap();
            std::fs::write(d.join("f.bin"), b"x").unwrap();
        }
        sweep_stale_uploads();
        assert!(!dead.exists(), "死 pid 副本目录被清扫");
        assert!(ours.exists(), "本 pid 目录保留");
        assert!(live.exists(), "存活 pid（init=1）目录保留");
        assert!(alien.exists(), "非 pid 命名目录不动");
        for d in [ours, live, alien] {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}
