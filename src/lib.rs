#![cfg(target_os = "linux")]
pub mod auth;
pub mod config;
pub mod daemon;
pub mod keyboard;
pub mod layout;
pub mod memory;
pub mod socket;
pub mod state;

pub const MAX_PAYLOAD: usize = 1_048_576;
pub const CHUNK: usize = 16 * 1024;
pub type Result<T> = std::result::Result<T, &'static str>;

pub fn protect_process() -> Result<()> {
    // No secret data has been read when this function runs.
    unsafe {
        let limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if libc::setrlimit(libc::RLIMIT_CORE, &limit) != 0
            || libc::prctl(libc::PR_SET_DUMPABLE, 0) != 0
        {
            return Err("cannot disable process dumps");
        }
    }
    // Avoid third-party panic payloads accidentally including sensitive data.
    std::panic::set_hook(Box::new(|_| eprintln!("viska: internal failure")));
    Ok(())
}

pub fn boottime() -> std::time::Duration {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // CLOCK_BOOTTIME is present on the supported Linux kernels. Failure is fatal.
    if unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut ts) } != 0 {
        std::process::abort();
    }
    std::time::Duration::new(ts.tv_sec as u64, ts.tv_nsec as u32)
}
