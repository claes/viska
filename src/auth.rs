use std::{
    ffi::CString,
    sync::mpsc::{self, Receiver, SyncSender, TrySendError},
};

pub fn resolve(name: &str) -> Option<libc::uid_t> {
    let name = CString::new(name).ok()?;
    let mut size = 1024;
    while size <= 65_536 {
        let mut buffer = vec![0u8; size];
        let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut result = std::ptr::null_mut();
        let rc = unsafe {
            libc::getpwnam_r(
                name.as_ptr(),
                entry.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                size,
                &mut result,
            )
        };
        if rc == libc::ERANGE {
            size *= 2;
            continue;
        }
        if rc != 0 || result.is_null() {
            return None;
        }
        return Some(unsafe { entry.assume_init() }.pw_uid);
    }
    None
}
pub fn allowed(names: &[String], uid: libc::uid_t) -> bool {
    let mut matched = false;
    for name in names {
        match resolve(name) {
            Some(id) => matched |= id == uid,
            None => return false,
        }
    }
    matched
}
// One worker, a bounded queue, and per-client deadlines. A stuck NSS call cannot
// consume more threads or block the state loop. Restart is required to recover
// from a permanently stuck NSS module; no authorization is cached.
pub struct Resolver {
    tx: SyncSender<(u64, libc::uid_t, Vec<String>)>,
    pub rx: Receiver<(u64, bool)>,
}
impl Default for Resolver {
    fn default() -> Self {
        Self::new()
    }
}
impl Resolver {
    pub fn new() -> Self {
        let (tx, requests) = mpsc::sync_channel::<(u64, libc::uid_t, Vec<String>)>(8);
        let (results, rx) = mpsc::sync_channel(8);
        std::thread::spawn(move || {
            while let Ok((id, uid, names)) = requests.recv() {
                if results.send((id, allowed(&names, uid))).is_err() {
                    break;
                }
            }
        });
        Self { tx, rx }
    }
    pub fn submit(&self, id: u64, uid: libc::uid_t, names: Vec<String>) -> bool {
        match self.tx.try_send((id, uid, names)) {
            Ok(()) => true,
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => false,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn absent_and_empty_accounts_deny() {
        assert!(!allowed(&[], unsafe { libc::getuid() }));
        assert!(!allowed(
            &["viska-test-account-that-does-not-exist".into()],
            0
        ));
        assert!(!allowed(&["bad\0name".into()], 0));
        assert_eq!(resolve("root"), Some(0));
        assert!(allowed(&["root".into()], 0));
        assert!(!allowed(&["root".into()], 12345));
    }
}
