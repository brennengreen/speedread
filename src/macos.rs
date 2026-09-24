//! macOS process tuning, applied once at startup.
//!
//! * iCloud "dataless" placeholders (Desktop & Documents sync) are never
//!   materialized: opening one fails fast with `EDEADLK` instead of blocking
//!   on a download (Apple TN3150).
//! * Reads never update atime.
//! * The fd soft limit inherited from launchd (256 for GUI-spawned agents) is
//!   raised.
//! * Threads run at `QOS_CLASS_USER_INITIATED` so the scheduler prefers
//!   performance cores — an agent is blocked waiting on every call.

#[cfg(target_os = "macos")]
mod imp {
    const IOPOL_TYPE_VFS_ATIME_UPDATES: libc::c_int = 2;
    const IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES: libc::c_int = 3;
    const IOPOL_SCOPE_PROCESS: libc::c_int = 0;
    const IOPOL_ATIME_UPDATES_OFF: libc::c_int = 1;
    const IOPOL_MATERIALIZE_DATALESS_FILES_OFF: libc::c_int = 1;

    unsafe extern "C" {
        fn setiopolicy_np(
            iotype: libc::c_int,
            scope: libc::c_int,
            policy: libc::c_int,
        ) -> libc::c_int;
    }

    pub fn tune_process() {
        // SAFETY: plain libc calls with constant arguments.
        unsafe {
            setiopolicy_np(
                IOPOL_TYPE_VFS_MATERIALIZE_DATALESS_FILES,
                IOPOL_SCOPE_PROCESS,
                IOPOL_MATERIALIZE_DATALESS_FILES_OFF,
            );
            setiopolicy_np(
                IOPOL_TYPE_VFS_ATIME_UPDATES,
                IOPOL_SCOPE_PROCESS,
                IOPOL_ATIME_UPDATES_OFF,
            );
            let mut rl = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            if libc::getrlimit(libc::RLIMIT_NOFILE, &mut rl) == 0 {
                let want: libc::rlim_t = 10240;
                if rl.rlim_cur < want {
                    rl.rlim_cur = want.min(rl.rlim_max);
                    libc::setrlimit(libc::RLIMIT_NOFILE, &rl);
                }
            }
        }
        set_thread_qos();
    }

    pub fn set_thread_qos() {
        // SAFETY: affects only the calling thread's scheduling class.
        unsafe {
            libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INITIATED, 0);
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    pub fn tune_process() {}
    pub fn set_thread_qos() {}
}

pub use imp::{set_thread_qos, tune_process};

/// Worker threads: the performance-core count on Apple Silicon.
///
/// File-system-heavy work (walking, searching) *slows down* once threads
/// spill onto efficiency cores: on an M4 (4P+6E), a ripgrep-style search of
/// 19k files takes 104 ms with 4 threads but 284 ms with 10, as kernel time
/// balloons from lock contention. `SPEEDREAD_THREADS` overrides.
pub fn worker_threads() -> usize {
    if let Some(n) = std::env::var("SPEEDREAD_THREADS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        && n > 0
    {
        return n;
    }
    let all = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(8);
    perf_cores().map(|p| p.clamp(2, all)).unwrap_or(all).min(16)
}

#[cfg(target_os = "macos")]
fn perf_cores() -> Option<usize> {
    let mut n: libc::c_int = 0;
    let mut len = std::mem::size_of::<libc::c_int>();
    // SAFETY: valid NUL-terminated name and correctly sized out-buffer.
    let r = unsafe {
        libc::sysctlbyname(
            c"hw.perflevel0.logicalcpu".as_ptr(),
            &mut n as *mut libc::c_int as *mut libc::c_void,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    (r == 0 && n > 0).then_some(n as usize)
}

#[cfg(not(target_os = "macos"))]
fn perf_cores() -> Option<usize> {
    None
}

/// Configure the global rayon pool (worker threads inherit the QoS class).
pub fn init_thread_pool() {
    let _ = rayon::ThreadPoolBuilder::new()
        .num_threads(worker_threads())
        .thread_name(|i| format!("speedread-{i}"))
        .start_handler(|_| set_thread_qos())
        .build_global();
}
