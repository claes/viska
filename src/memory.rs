use crate::Result;
use std::ptr::NonNull;
use zeroize::Zeroize;

// mmap gives stable, dedicated pages; no heap reallocations or allocator sharing.
// No Debug/Clone/Serialize implementation may be added to this type.
pub struct Secret {
    ptr: NonNull<u8>,
    capacity: usize,
    mapped: usize,
    len: usize,
}
impl Secret {
    pub fn new(capacity: usize) -> Result<Self> {
        Self::allocate(capacity, false)
    }
    fn allocate(capacity: usize, fail_lock: bool) -> Result<Self> {
        if capacity == 0 || capacity > crate::MAX_PAYLOAD + crate::CHUNK + 64 {
            return Err("invalid buffer capacity");
        }
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
        let mapped = capacity.div_ceil(page) * page;
        unsafe {
            let p = libc::mmap(
                std::ptr::null_mut(),
                mapped,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            );
            if p == libc::MAP_FAILED {
                return Err("cannot allocate protected memory");
            }
            if fail_lock || libc::mlock(p, mapped) != 0 {
                libc::munmap(p, mapped);
                return Err("cannot lock sensitive memory; check RLIMIT_MEMLOCK");
            }
            if libc::madvise(p, mapped, libc::MADV_DONTDUMP) != 0 {
                libc::munlock(p, mapped);
                libc::munmap(p, mapped);
                return Err("cannot exclude sensitive memory from dumps");
            }
            Ok(Self {
                ptr: NonNull::new(p.cast()).unwrap(),
                capacity,
                mapped,
                len: 0,
            })
        }
    }
    pub fn bytes(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }
    pub fn bytes_mut(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }
    pub fn spare(&mut self) -> &mut [u8] {
        unsafe {
            std::slice::from_raw_parts_mut(
                self.ptr.as_ptr().add(self.len),
                self.capacity - self.len,
            )
        }
    }
    pub fn set_len(&mut self, n: usize) -> Result<()> {
        if n > self.capacity {
            return Err("buffer limit exceeded");
        }
        self.len = n;
        Ok(())
    }
    pub fn append(&mut self, bytes: &[u8]) -> Result<()> {
        if bytes.len() > self.capacity - self.len {
            return Err("buffer limit exceeded");
        }
        self.spare()[..bytes.len()].copy_from_slice(bytes);
        self.len += bytes.len();
        Ok(())
    }
    pub fn clear(&mut self) {
        unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.mapped) }.zeroize();
        self.len = 0;
    }
}
impl Drop for Secret {
    fn drop(&mut self) {
        self.clear();
        unsafe {
            libc::munlock(self.ptr.as_ptr().cast(), self.mapped);
            libc::munmap(self.ptr.as_ptr().cast(), self.mapped);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wipe_entire_allocation_and_fail_closed() {
        let mut b = Secret::new(100).unwrap();
        b.append(b"nonsensitive test").unwrap();
        b.clear();
        assert!(b.bytes().is_empty());
        assert!(b.spare().iter().all(|v| *v == 0));
        assert!(Secret::allocate(100, true).is_err());
        assert!(b.set_len(101).is_err());
    }
}
