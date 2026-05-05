//! iOS process memory diagnostics.
//!
//! On iOS the OOM killer (jetsam) terminates apps that exceed their memory budget without
//! sending any signal that we can catch from Rust. To diagnose that situation post-mortem we
//! sample the resident set size and the remaining memory budget every frame, so the last lines
//! in our log file expose how close we got to the limit before being killed.
//!
//! - `task_info` with `MACH_TASK_BASIC_INFO` returns `resident_size` (RSS, bytes).
//! - `os_proc_available_memory()` returns the remaining "soft limit" memory in bytes; iOS
//!   jetsams the process when this approaches zero.

use std::os::raw::{c_int, c_uint};

#[allow(non_camel_case_types)]
type natural_t = c_uint;
#[allow(non_camel_case_types)]
type integer_t = c_int;
#[allow(non_camel_case_types)]
type mach_port_t = c_uint;
#[allow(non_camel_case_types)]
type kern_return_t = c_int;
#[allow(non_camel_case_types)]
type mach_msg_type_number_t = natural_t;
#[allow(non_camel_case_types)]
type task_flavor_t = natural_t;
#[allow(non_camel_case_types)]
type policy_t = c_int;
#[allow(non_camel_case_types)]
type time_value_t = [integer_t; 2];

#[repr(C)]
#[derive(Default, Clone, Copy)]
#[allow(non_camel_case_types)]
struct mach_task_basic_info {
    virtual_size: u64,
    resident_size: u64,
    resident_size_max: u64,
    user_time: time_value_t,
    system_time: time_value_t,
    policy: policy_t,
    suspend_count: integer_t,
}

const MACH_TASK_BASIC_INFO: task_flavor_t = 20;
const MACH_TASK_BASIC_INFO_COUNT: mach_msg_type_number_t =
    (core::mem::size_of::<mach_task_basic_info>() / core::mem::size_of::<integer_t>())
        as mach_msg_type_number_t;

unsafe extern "C" {
    fn mach_task_self() -> mach_port_t;
    fn task_info(
        task: mach_port_t,
        flavor: task_flavor_t,
        info_out: *mut integer_t,
        info_out_count: *mut mach_msg_type_number_t,
    ) -> kern_return_t;
    fn os_proc_available_memory() -> usize;
}

/// Returns `(resident_bytes, available_bytes)` for the current process.
/// `available_bytes` is the iOS soft-limit headroom; when it nears zero, jetsam kills the app.
pub fn snapshot() -> Option<(u64, u64)> {
    let mut info = mach_task_basic_info::default();
    let mut count = MACH_TASK_BASIC_INFO_COUNT;
    let kr = unsafe {
        task_info(
            mach_task_self(),
            MACH_TASK_BASIC_INFO,
            &mut info as *mut _ as *mut integer_t,
            &mut count,
        )
    };
    if kr != 0 {
        return None;
    }
    let avail = unsafe { os_proc_available_memory() } as u64;
    Some((info.resident_size, avail))
}

/// Format `(resident, available)` snapshot as `"rss=XX.XMB headroom=YY.YMB"`.
pub fn format_snapshot() -> String {
    match snapshot() {
        Some((rss, avail)) => format!(
            "rss={:.1}MB headroom={:.1}MB",
            rss as f64 / (1024.0 * 1024.0),
            avail as f64 / (1024.0 * 1024.0),
        ),
        None => "mem_probe: unavailable".to_string(),
    }
}
