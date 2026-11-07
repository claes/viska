use crate::{
    config::Backend,
    layout::{Layout, Stroke},
    memory::Secret,
    Result,
};
use std::{
    fs::{File, OpenOptions},
    os::{fd::AsRawFd, unix::fs::OpenOptionsExt},
};

const SHIFT: u16 = 42;
const ALTGR: u16 = 100;
const EVENT_SIZE: usize = std::mem::size_of::<libc::input_event>();
#[repr(C)]
struct InputId {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
}
#[repr(C)]
struct Setup {
    id: InputId,
    name: [u8; 80],
    ff_effects_max: u32,
}

pub struct Keyboard {
    file: Option<File>,
    events: Secret,
    held: Secret,
    cursor: usize,
    ready: bool,
    #[cfg(test)]
    fail_after: Option<usize>,
}
impl Keyboard {
    pub fn new(backend: Backend, layout: &Layout) -> Result<Self> {
        let mut keyboard = Self {
            file: None,
            events: Secret::new(16 * EVENT_SIZE)?,
            held: Secret::new(128)?,
            cursor: 0,
            ready: true,
            #[cfg(test)]
            fail_after: None,
        };
        keyboard.held.set_len(128)?;
        if backend == Backend::Uinput {
            let file = OpenOptions::new()
                .write(true)
                .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
                .open("/dev/uinput")
                .map_err(|_| "cannot open /dev/uinput")?;
            let fd = file.as_raw_fd();
            // Linux UAPI: _IOW('U', 100/101, int), _IOW('U', 3, uinput_setup).
            let ioctl = |request: libc::c_ulong, value: i32| {
                if unsafe { libc::ioctl(fd, request, value) } < 0 {
                    Err("uinput capability setup failed")
                } else {
                    Ok(())
                }
            };
            ioctl(0x40045564, 1)?; // UI_SET_EVBIT(EV_KEY)
            for key in layout.keys().chain([SHIFT, ALTGR]) {
                ioctl(0x40045565, key as i32)?;
            }
            let mut setup = Setup {
                id: InputId {
                    bustype: 0x03,
                    vendor: 0,
                    product: 0,
                    version: 1,
                },
                name: [0; 80],
                ff_effects_max: 0,
            };
            setup.name[..5].copy_from_slice(b"viska");
            if unsafe { libc::ioctl(fd, 0x405c5503 as libc::c_ulong, &setup) } < 0
                || unsafe { libc::ioctl(fd, 0x5501 as libc::c_ulong) } < 0
            {
                return Err("cannot register virtual keyboard");
            }
            keyboard.file = Some(file);
        }
        Ok(keyboard)
    }
    pub fn ready(&self) -> bool {
        self.ready
    }
    pub fn idle(&self) -> bool {
        self.cursor == self.events.bytes().len()
    }
    fn push(&mut self, kind: u16, key: u16, value: i32) -> Result<()> {
        if self.events.spare().len() < EVENT_SIZE {
            return Err("event buffer limit");
        }
        unsafe {
            // Write directly into locked memory; no secret-bearing heap array.
            let ptr = self.events.spare().as_mut_ptr().cast::<libc::input_event>();
            std::ptr::write_bytes(ptr, 0, 1);
            (*ptr).type_ = kind;
            (*ptr).code = key;
            (*ptr).value = value;
        }
        self.events.set_len(self.events.bytes().len() + EVENT_SIZE)
    }
    fn key(&mut self, code: u16, value: i32) -> Result<()> {
        self.push(1, code, value)?;
        self.push(0, 0, 0)
    }
    pub fn prepare(&mut self, mut stroke: Stroke) -> Result<()> {
        if !self.ready || !self.idle() {
            return Err("keyboard unavailable");
        }
        self.events.clear();
        self.cursor = 0;
        let result = (|| {
            if stroke.modifiers & 1 != 0 {
                self.key(SHIFT, 1)?;
            }
            if stroke.modifiers & 2 != 0 {
                self.key(ALTGR, 1)?;
            }
            self.key(stroke.key, 1)?;
            self.key(stroke.key, 0)?;
            if stroke.modifiers & 2 != 0 {
                self.key(ALTGR, 0)?;
            }
            if stroke.modifiers & 1 != 0 {
                self.key(SHIFT, 0)?;
            }
            Ok(())
        })();
        use zeroize::Zeroize;
        stroke.key.zeroize();
        stroke.modifiers.zeroize();
        result
    }
    pub fn tick(&mut self) -> Result<bool> {
        if !self.ready {
            return Err("keyboard disabled after failure");
        }
        if self.idle() {
            return Ok(true);
        }
        let slice = &self.events.bytes()[self.cursor..self.cursor + EVENT_SIZE];
        #[cfg(test)]
        if self.fail_after == Some(0) {
            return Err("simulated device failure");
        }
        if let Some(file) = &self.file {
            let n = unsafe { libc::write(file.as_raw_fd(), slice.as_ptr().cast(), EVENT_SIZE) };
            if n < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::WouldBlock {
                return Ok(false);
            }
            if n != EVENT_SIZE as isize {
                return Err("keyboard write failed; partial typing possible");
            }
        }
        // Track keys after each accepted event, including before its SYN_REPORT.
        let event = unsafe { &*slice.as_ptr().cast::<libc::input_event>() };
        if event.type_ == 1 {
            if event.code as usize >= 128 {
                return Err("invalid key event");
            }
            let old = self.held.bytes()[event.code as usize];
            if (event.value == 1 && old != 0) || (event.value == 0 && old != 1) {
                return Err("unbalanced key sequence");
            }
            self.held.bytes_mut()[event.code as usize] = event.value as u8;
        }
        #[cfg(test)]
        if let Some(n) = self.fail_after.as_mut() {
            *n -= 1;
        }
        self.cursor += EVENT_SIZE;
        if self.idle() {
            self.events.clear();
            self.cursor = 0;
        }
        Ok(true)
    }
    pub fn release(&mut self) -> Result<()> {
        self.events.clear();
        self.cursor = 0;
        let mut failed = false;
        // No unbounded waits on the device. A failed cleanup invalidates it.
        for key in 0..128 {
            if self.held.bytes()[key] != 0 {
                self.events.clear();
                self.cursor = 0;
                self.key(key as u16, 0)?;
                while !self.idle() {
                    match self.tick() {
                        Ok(true) => (),
                        _ => {
                            failed = true;
                            break;
                        }
                    }
                }
            }
        }
        self.events.clear();
        self.cursor = 0;
        self.held.clear();
        self.held.set_len(128)?;
        if failed {
            self.ready = false;
            // Remove a failed device rather than leave a possibly held key
            // registered indefinitely. Recovery still requires a restart.
            if let Some(file) = self.file.take() {
                unsafe {
                    libc::ioctl(file.as_raw_fd(), 0x5502 as libc::c_ulong);
                }
            }
            return Err("key release failed; keyboard disabled");
        }
        Ok(())
    }
}
impl Drop for Keyboard {
    fn drop(&mut self) {
        let _ = self.release();
        if let Some(file) = &self.file {
            unsafe {
                libc::ioctl(file.as_raw_fd(), 0x5502 as libc::c_ulong);
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_character_balances_and_syn_boundaries() {
        for name in ["us", "se"] {
            let l = Layout::new(name).unwrap();
            let mut k = Keyboard::new(Backend::Mock, &l).unwrap();
            let source = if name == "us" {
                include_str!("../layouts/us.json")
            } else {
                include_str!("../layouts/se.json")
            };
            let rows: Vec<[u32; 3]> = serde_json::from_str(source).unwrap();
            for row in rows {
                k.prepare(l.lookup(char::from_u32(row[0]).unwrap()).unwrap())
                    .unwrap();
                for (i, ev) in k.events.bytes().chunks_exact(EVENT_SIZE).enumerate() {
                    let ev = unsafe { &*ev.as_ptr().cast::<libc::input_event>() };
                    assert_eq!(ev.type_, if i % 2 == 0 { 1 } else { 0 });
                }
                while !k.idle() {
                    k.tick().unwrap();
                }
                assert!(k.held.bytes().iter().all(|v| *v == 0));
            }
        }
    }
    #[test]
    fn failure_and_cancel_release() {
        let l = Layout::new("se").unwrap();
        let mut k = Keyboard::new(Backend::Mock, &l).unwrap();
        k.prepare(l.lookup('@').unwrap()).unwrap();
        k.tick().unwrap();
        assert_eq!(k.held.bytes()[ALTGR as usize], 1);
        k.release().unwrap();
        assert!(k.idle());
        assert!(k.held.bytes().iter().all(|v| *v == 0));
        k.prepare(l.lookup('@').unwrap()).unwrap();
        k.tick().unwrap();
        k.fail_after = Some(0);
        assert!(k.release().is_err());
        assert!(!k.ready());
    }
}
