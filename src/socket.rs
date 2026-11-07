use crate::Result;
use std::{
    ffi::OsStr,
    mem,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Path, PathBuf},
};

pub struct Socket(pub OwnedFd);
fn address(path: &OsStr) -> Result<(libc::sockaddr_un, libc::socklen_t)> {
    let bytes = path.as_bytes();
    let mut addr: libc::sockaddr_un = unsafe { mem::zeroed() };
    if bytes.is_empty() || bytes.len() >= addr.sun_path.len() || bytes.contains(&0) {
        return Err("invalid socket path");
    }
    addr.sun_family = libc::AF_UNIX as _;
    for (a, b) in addr.sun_path.iter_mut().zip(bytes) {
        *a = *b as _;
    }
    Ok((
        addr,
        (mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1) as _,
    ))
}
fn new() -> Result<Socket> {
    let fd = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_SEQPACKET | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            0,
        )
    };
    if fd < 0 {
        return Err("cannot create IPC socket");
    }
    let socket = Socket(unsafe { OwnedFd::from_raw_fd(fd) });
    let enabled: libc::c_int = 1;
    // Even an empty packet carries SCM_CREDENTIALS; EOF carries no packet.
    // This distinguishes zero-length records from shutdown(SHUT_WR).
    if unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PASSCRED,
            (&enabled as *const libc::c_int).cast(),
            mem::size_of_val(&enabled) as _,
        )
    } != 0
    {
        return Err("cannot enable IPC record credentials");
    }
    Ok(socket)
}
impl Socket {
    pub fn connect(path: &Path) -> Result<Self> {
        let s = new()?;
        let (addr, len) = address(path.as_os_str())?;
        if unsafe { libc::connect(s.fd(), (&addr as *const libc::sockaddr_un).cast(), len) } != 0 {
            return Err("cannot connect to daemon");
        }
        Ok(s)
    }
    pub fn fd(&self) -> i32 {
        self.0.as_raw_fd()
    }
    pub fn peer_uid(&self) -> Result<libc::uid_t> {
        let mut cred: libc::ucred = unsafe { mem::zeroed() };
        let mut len = mem::size_of_val(&cred) as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                self.fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut cred as *mut libc::ucred).cast(),
                &mut len,
            )
        } != 0
            || len as usize != mem::size_of_val(&cred)
        {
            return Err("cannot authenticate peer");
        }
        Ok(cred.uid)
    }
    // Receives into a locked caller-owned buffer. MSG_TRUNC must never be accepted.
    pub fn recv(&self, bytes: &mut [u8]) -> Result<Option<usize>> {
        let mut iov = libc::iovec {
            iov_base: bytes.as_mut_ptr().cast(),
            iov_len: bytes.len(),
        };
        let mut control = [0u64; 32]; // aligned metadata, never secret payload
        let mut message: libc::msghdr = unsafe { mem::zeroed() };
        message.msg_iov = &mut iov;
        message.msg_iovlen = 1;
        message.msg_control = control.as_mut_ptr().cast();
        message.msg_controllen = mem::size_of_val(&control);
        let n = unsafe {
            libc::recvmsg(
                self.fd(),
                &mut message,
                libc::MSG_TRUNC | libc::MSG_DONTWAIT | libc::MSG_CMSG_CLOEXEC,
            )
        };
        if n < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::WouldBlock
                || e.kind() == std::io::ErrorKind::Interrupted
            {
                return Ok(None);
            }
            return Err("IPC receive failed");
        }
        let mut record = false;
        let mut unexpected = false;
        unsafe {
            let mut cmsg = libc::CMSG_FIRSTHDR(&message);
            while !cmsg.is_null() {
                if (*cmsg).cmsg_level == libc::SOL_SOCKET
                    && (*cmsg).cmsg_type == libc::SCM_CREDENTIALS
                {
                    record = true;
                } else {
                    unexpected = true;
                    if (*cmsg).cmsg_level == libc::SOL_SOCKET
                        && (*cmsg).cmsg_type == libc::SCM_RIGHTS
                    {
                        // Never retain caller-supplied descriptors. The kernel
                        // closes any excess descriptors truncated out of this buffer.
                        let count =
                            ((*cmsg).cmsg_len - libc::CMSG_LEN(0) as usize) / mem::size_of::<i32>();
                        let fds = libc::CMSG_DATA(cmsg).cast::<i32>();
                        for i in 0..count {
                            libc::close(*fds.add(i));
                        }
                    }
                }
                cmsg = libc::CMSG_NXTHDR(&message, cmsg);
            }
        }
        if unexpected || message.msg_flags & libc::MSG_CTRUNC != 0 {
            return Err("unexpected IPC ancillary data");
        }
        if n == 0 && !record {
            return Err("peer disconnected");
        }
        if n == 0 {
            return Err("empty IPC packet");
        }
        if n as usize > bytes.len() || message.msg_flags & libc::MSG_TRUNC != 0 {
            return Err("truncated IPC packet");
        }
        Ok(Some(n as usize))
    }
    pub fn send(&self, bytes: &[u8]) -> Result<bool> {
        let n = unsafe {
            libc::send(
                self.fd(),
                bytes.as_ptr().cast(),
                bytes.len(),
                libc::MSG_NOSIGNAL | libc::MSG_DONTWAIT,
            )
        };
        if n < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::WouldBlock
                || e.kind() == std::io::ErrorKind::Interrupted
            {
                return Ok(false);
            }
            return Err("IPC send failed");
        }
        if n as usize != bytes.len() {
            return Err("incomplete IPC send");
        }
        Ok(true)
    }
    pub fn wait(&self, events: i16, timeout_ms: i32) -> Result<()> {
        let mut fd = libc::pollfd {
            fd: self.fd(),
            events,
            revents: 0,
        };
        let n = unsafe { libc::poll(&mut fd, 1, timeout_ms) };
        if n <= 0 {
            return Err("IPC timeout or interruption; request outcome may be uncertain");
        }
        Ok(())
    }
    pub fn send_wait(&self, data: &[u8]) -> Result<()> {
        let end = crate::boottime() + std::time::Duration::from_secs(5);
        loop {
            if self.send(data)? {
                return Ok(());
            }
            if crate::boottime() >= end {
                return Err("IPC send timeout; request outcome may be uncertain");
            }
            // A short poll timeout is not the request's five-second deadline.
            let _ = self.wait(libc::POLLOUT, 100);
        }
    }
}
pub struct Listener {
    socket: Socket,
    path: PathBuf,
    inode: u64,
}
impl Listener {
    pub fn bind(path: &Path) -> Result<Self> {
        // Every ancestor must be trusted, with a sticky root-owned /tmp allowed
        // for the demo. Reject symlink ancestors, even when the leaf is private.
        let parent = path.parent().ok_or("invalid endpoint parent")?;
        for ancestor in parent.ancestors() {
            let m = std::fs::symlink_metadata(ancestor).map_err(|_| "unsafe endpoint ancestor")?;
            let trusted_owner = m.uid() == 0 || m.uid() == unsafe { libc::geteuid() };
            let sticky_root = m.uid() == 0 && m.mode() & libc::S_ISVTX != 0;
            if !m.is_dir() || !trusted_owner || (m.mode() & 0o022 != 0 && !sticky_root) {
                return Err("unsafe endpoint ancestor");
            }
        }
        let m = std::fs::symlink_metadata(parent).map_err(|_| "runtime directory missing")?;
        if !m.is_dir()
            || m.mode() & 0o022 != 0
            || (m.uid() != 0 && m.uid() != unsafe { libc::geteuid() })
        {
            return Err("unsafe runtime directory owner or permissions");
        }
        if let Ok(m) = std::fs::symlink_metadata(path) {
            use std::os::unix::fs::FileTypeExt;
            if !m.file_type().is_socket() || m.uid() != unsafe { libc::geteuid() } {
                return Err("unexpected endpoint object");
            }
            let probe = new()?;
            let (addr, len) = address(path.as_os_str())?;
            let rc = unsafe {
                libc::connect(probe.fd(), (&addr as *const libc::sockaddr_un).cast(), len)
            };
            let error = std::io::Error::last_os_error();
            if rc == 0 {
                return Err("endpoint already active");
            }
            // Only remove an owned socket with ECONNREFUSED, never unknown errors.
            if error.raw_os_error() != Some(libc::ECONNREFUSED) {
                return Err("cannot verify stale endpoint");
            }
            std::fs::remove_file(path).map_err(|_| "cannot remove stale endpoint")?;
        }
        let socket = new()?;
        let (addr, len) = address(path.as_os_str())?;
        if unsafe { libc::bind(socket.fd(), (&addr as *const libc::sockaddr_un).cast(), len) } != 0
        {
            return Err("cannot bind endpoint");
        }
        let inode = std::fs::symlink_metadata(path)
            .map_err(|_| "cannot inspect endpoint")?
            .ino();
        let listener = Self {
            socket,
            path: path.into(),
            inode,
        };
        // Owner and service-managed access group only; no world access.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o660))
            .map_err(|_| "cannot restrict endpoint")?;
        if unsafe { libc::listen(listener.fd(), 16) } != 0 {
            return Err("cannot listen on endpoint");
        }
        Ok(listener)
    }
    pub fn fd(&self) -> i32 {
        self.socket.fd()
    }
    pub fn accept(&self) -> Result<Option<Socket>> {
        let fd = unsafe {
            libc::accept4(
                self.fd(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            )
        };
        if fd < 0 {
            if std::io::Error::last_os_error().kind() == std::io::ErrorKind::WouldBlock {
                return Ok(None);
            }
            return Err("cannot accept connection");
        }
        Ok(Some(Socket(unsafe { OwnedFd::from_raw_fd(fd) })))
    }
}
impl Drop for Listener {
    fn drop(&mut self) {
        if std::fs::symlink_metadata(&self.path).is_ok_and(|m| m.ino() == self.inode) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}
