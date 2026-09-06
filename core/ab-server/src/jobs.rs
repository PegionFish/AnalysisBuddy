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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

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

/// 任务注册表 + 并发闸（spawn 后一切在 [`JobRegistry::run_import`] 内推进）。
pub struct JobRegistry {
    next_id: AtomicU64,
    semaphore: Arc<Semaphore>,
    jobs: Mutex<HashMap<String, Job>>,
    cancel_flags: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

impl JobRegistry {
    pub fn new(max_concurrent_imports: usize) -> Self {
        Self {
            next_id: AtomicU64::new(1),
            semaphore: Arc::new(Semaphore::new(max_concurrent_imports.max(1))),
            jobs: Mutex::new(HashMap::new()),
            cancel_flags: Mutex::new(HashMap::new()),
        }
    }

    /// 登记并 spawn 一个导入任务，返回 queued 快照（202 响应体）。
    pub fn spawn_import(
        self: &Arc<Self>,
        coordinator: Arc<ImportCoordinator>,
        paths: Vec<String>,
        overrides: Option<HashMap<String, ImportOverride>>,
    ) -> JobStatusDto {
        let job_id = format!("job-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        self.jobs.lock().expect("jobs lock").insert(
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
            .expect("flags lock")
            .insert(job_id.clone(), flag.clone());
        let registry = Arc::clone(self);
        let job_for_task = job_id.clone();
        tokio::spawn(async move {
            registry
                .run_import(job_for_task, coordinator, paths, overrides, flag)
                .await;
        });
        self.status(&job_id).expect("job just inserted")
    }

    async fn run_import(
        self: Arc<Self>,
        job_id: String,
        coordinator: Arc<ImportCoordinator>,
        paths: Vec<String>,
        overrides: Option<HashMap<String, ImportOverride>>,
        flag: Arc<AtomicBool>,
    ) {
        let mut files: Vec<ImportResultDto> = Vec::new();
        let mut error: Option<IpcError> = None;
        // 排队等并发闸（queued 期间被取消则直接终态，permit 自动释放）。
        let permit = self.semaphore.clone().acquire_owned().await;
        if flag.load(Ordering::SeqCst) {
            drop(permit);
            self.finish(&job_id, JobState::Cancelled, files, None);
            return;
        }
        let Ok(permit) = permit else {
            return; // semaphore 关闭：不发生（registry 存活期间不 drop）
        };
        self.mark_running(&job_id);

        let mut stopped = false;
        for path in paths {
            if flag.load(Ordering::SeqCst) {
                stopped = true;
                break;
            }
            // per-path overrides（与桌面一致：按路径键查找手选覆盖）。
            let own_overrides = overrides
                .as_ref()
                .and_then(|map| map.get(&path).cloned())
                .map(|entry| HashMap::from([(path.clone(), entry)]));
            match import_files_logic(&coordinator, vec![path], own_overrides).await {
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
        self.finish(&job_id, state, files, error);
    }

    /// 取消任务：置旗标（running 任务在文件边界停止）；queued 任务就地
    /// 翻转 Cancelled。未知 job_id → None（404）。
    pub fn cancel(&self, job_id: &str) -> Option<JobStatusDto> {
        {
            // Arc<AtomicBool> 经共享引用即可 store（无需 mut 绑定）。
            let flags = self.cancel_flags.lock().expect("flags lock");
            let flag = flags.get(job_id)?;
            flag.store(true, Ordering::SeqCst);
        }
        let mut jobs = self.jobs.lock().expect("jobs lock");
        let job = jobs.get_mut(job_id)?;
        if job.state == JobState::Queued {
            job.state = JobState::Cancelled;
        }
        Some(snapshot_of(job_id, job))
    }

    /// 当前任务状态（未知 job_id → None）。
    pub fn status(&self, job_id: &str) -> Option<JobStatusDto> {
        let jobs = self.jobs.lock().expect("jobs lock");
        let job = jobs.get(job_id)?;
        Some(snapshot_of(job_id, job))
    }

    fn mark_running(&self, job_id: &str) {
        let mut jobs = self.jobs.lock().expect("jobs lock");
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
            let mut jobs = self.jobs.lock().expect("jobs lock");
            if let Some(job) = jobs.get_mut(job_id) {
                if job.state != JobState::Cancelled || state == JobState::Cancelled {
                    job.state = state;
                    job.files = files;
                    job.error = error;
                }
            }
        }
        self.cancel_flags.lock().expect("flags lock").remove(job_id);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn manual_job(registry: &JobRegistry, job_id: &str) {
        registry.jobs.lock().expect("jobs lock").insert(
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
            .expect("flags lock")
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
        // 旗标在任务收尾（run_import → finish）时才清理：模拟收尾后
        // 再次 cancel → 未知。
        registry.finish("job-7", JobState::Cancelled, Vec::new(), None);
        assert!(registry.cancel("job-7").is_none());
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
}
