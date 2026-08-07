//! Windows RSS 采样器（qa-perf.md §4.2 指标 2）：`K32GetProcessMemoryInfo` 直读
//! `WorkingSetSize`，200ms 间隔捕获秒级峰值；与 `tests/perf/rss_probe.ps1`
//! （`System.Diagnostics.Process.WorkingSet64` 同口径）双路互验，偏差 ≤5%。

use std::ffi::c_void;
use std::time::{Duration, Instant};

/// PROCESS_MEMORY_COUNTERS（PSAPI）。
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct ProcessMemoryCounters {
    cb: u32,
    page_fault_count: u32,
    peak_working_set_size: usize,
    working_set_size: usize,
    quota_peak_paged_pool_usage: usize,
    quota_paged_pool_usage: usize,
    quota_peak_non_paged_pool_usage: usize,
    quota_non_paged_pool_usage: usize,
    pagefile_usage: usize,
    peak_pagefile_usage: usize,
}

const PROCESS_QUERY_INFORMATION: u32 = 0x0400;
const PROCESS_VM_READ: u32 = 0x0010;

#[link(name = "kernel32")]
extern "system" {
    fn OpenProcess(dw_desired_access: u32, b_inherit_handle: i32, dw_process_id: u32) -> *mut c_void;
    fn K32GetProcessMemoryInfo(
        h_process: *mut c_void,
        counters: *mut ProcessMemoryCounters,
        cb: u32,
    ) -> i32;
    fn GetCurrentProcess() -> *mut c_void;
    fn CloseHandle(h_object: *mut c_void) -> i32;
}

/// 目标进程句柄封装（RAII：释放时 CloseHandle）。
pub struct ProcessHandle {
    handle: *mut c_void,
    own: bool,
}

impl ProcessHandle {
    /// 打开外部进程（需查询/读内存权限）。
    pub fn open(pid: u32) -> Result<Self, String> {
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, 0, pid);
            if h.is_null() {
                return Err(format!("OpenProcess({pid}) 失败"));
            }
            Ok(Self { handle: h, own: true })
        }
    }

    /// 当前进程（伪句柄，不关闭）。
    pub fn current() -> Self {
        unsafe {
            Self {
                handle: GetCurrentProcess(),
                own: false,
            }
        }
    }

    /// 当前工作集（字节）。
    pub fn working_set_bytes(&self) -> Result<u64, String> {
        let mut c = ProcessMemoryCounters {
            cb: std::mem::size_of::<ProcessMemoryCounters>() as u32,
            ..Default::default()
        };
        let ok = unsafe { K32GetProcessMemoryInfo(self.handle, &mut c, c.cb) };
        if ok == 0 {
            return Err("K32GetProcessMemoryInfo 失败".to_string());
        }
        Ok(c.working_set_size as u64)
    }

    /// 峰值工作集（字节）。
    pub fn peak_working_set_bytes(&self) -> Result<u64, String> {
        let mut c = ProcessMemoryCounters {
            cb: std::mem::size_of::<ProcessMemoryCounters>() as u32,
            ..Default::default()
        };
        let ok = unsafe { K32GetProcessMemoryInfo(self.handle, &mut c, c.cb) };
        if ok == 0 {
            return Err("K32GetProcessMemoryInfo 失败".to_string());
        }
        Ok(c.peak_working_set_size as u64)
    }
}

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        if self.own && !self.handle.is_null() {
            unsafe {
                CloseHandle(self.handle);
            }
        }
    }
}

/// 200ms 间隔循环采样峰值（qa-perf.md §4.2：`while alive { Refresh(); Max(peak, WS); sleep 200ms }`）。
/// `deadline` 到时或进程消失即停。返回 (峰值 MB, 采样点数)。
pub fn trace_peak(pid: u32, duration: Duration, interval: Duration) -> Result<(f64, usize), String> {
    let handle = ProcessHandle::open(pid)?;
    let deadline = Instant::now() + duration;
    let mut peak = 0u64;
    let mut samples = 0usize;
    while Instant::now() < deadline {
        match handle.working_set_bytes() {
            Ok(ws) => {
                peak = peak.max(ws);
                samples += 1;
            }
            Err(_) => break, // 进程已退出
        }
        std::thread::sleep(interval);
    }
    Ok((peak as f64 / (1024.0 * 1024.0), samples))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_process_sampler_returns_sane_working_set() {
        let h = ProcessHandle::current();
        let ws = h.working_set_bytes().expect("sample current process");
        assert!(ws > 0, "当前进程工作集必须 > 0");
        let peak = h.peak_working_set_bytes().expect("peak");
        assert!(peak >= ws);
    }
}
