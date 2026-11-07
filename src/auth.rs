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
enum Job {
    Authorize(u64, libc::uid_t, Vec<String>, Option<String>),
    Recipient(u64, String),
}
pub struct Resolver {
    tx: SyncSender<Job>,
    pub rx: Receiver<(u64, bool)>,
}
impl Default for Resolver {
    fn default() -> Self {
        Self::new()
    }
}
impl Resolver {
    pub fn new() -> Self {
        let (tx, requests) = mpsc::sync_channel::<Job>(8);
        let (results, rx) = mpsc::sync_channel(8);
        std::thread::spawn(move || {
            while let Ok(job) = requests.recv() {
                let (id, pass) = match job {
                    Job::Authorize(id, uid, names, recipient) => (
                        id,
                        allowed(&names, uid)
                            && recipient
                                .as_deref()
                                .is_none_or(|name| resolve(name) == Some(uid)),
                    ),
                    Job::Recipient(id, name) => (id, resolve(&name).is_some()),
                };
                if results.send((id, pass)).is_err() {
                    break;
                }
            }
        });
        Self { tx, rx }
    }
    fn enqueue(&self, job: Job) -> bool {
        match self.tx.try_send(job) {
            Ok(()) => true,
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => false,
        }
    }
    pub fn submit(&self, id: u64, uid: libc::uid_t, names: Vec<String>) -> bool {
        self.authorize(id, uid, names, None)
    }
    pub fn authorize(
        &self,
        id: u64,
        uid: libc::uid_t,
        names: Vec<String>,
        recipient: Option<String>,
    ) -> bool {
        self.enqueue(Job::Authorize(id, uid, names, recipient))
    }
    pub fn recipient(&self, id: u64, name: String) -> bool {
        self.enqueue(Job::Recipient(id, name))
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
