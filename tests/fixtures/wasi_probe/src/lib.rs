//! Preview-1 probe fixture. Imports exactly the three preview-1 functions
//! the host gates behind capabilities, declared by hand so the import set is
//! the test's and not libc's, and answers through the same packed-envelope
//! ABI as every other plugin.
//!
//! The bump allocator hands out nothing until `_initialize` has run: a host
//! that skips a reactor's initializer sees `alloc` return null on its first
//! write, which is the failure the fixture exists to expose.
#![no_std]

use core::fmt::Write;

#[link(wasm_import_module = "wasi_snapshot_preview1")]
extern "C" {
    fn clock_time_get(id: u32, precision: u64, out: *mut u64) -> u16;
    fn random_get(buf: *mut u8, len: usize) -> u16;
    fn fd_write(fd: u32, iovs: *const Ciovec, iovs_len: usize, nwritten: *mut usize) -> u16;
}

#[repr(C)]
struct Ciovec {
    buf: *const u8,
    len: usize,
}

const HEAP_SIZE: usize = 65_536;
static mut HEAP: [u8; HEAP_SIZE] = [0; HEAP_SIZE];
static mut HEAP_POS: usize = 0;
static mut INITIALIZED: bool = false;

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}

#[no_mangle]
pub extern "C" fn _initialize() {
    unsafe {
        INITIALIZED = true;
    }
}

#[no_mangle]
pub extern "C" fn alloc(size: u32) -> u32 {
    unsafe {
        if !INITIALIZED {
            return 0;
        }
        let pos = (HEAP_POS + 7) & !7;
        if pos + size as usize > HEAP_SIZE {
            return 0;
        }
        HEAP_POS = pos + size as usize;
        core::ptr::addr_of_mut!(HEAP).cast::<u8>().add(pos) as u32
    }
}

#[no_mangle]
pub extern "C" fn dealloc(_ptr: u32, _len: u32) {}

struct Out {
    ptr: u32,
    len: u32,
}

impl Out {
    fn new() -> Self {
        let ptr = alloc(512);
        Self { ptr, len: 0 }
    }

    fn packed(self) -> i64 {
        ((self.ptr as i64) << 32) | (self.len as i64)
    }
}

impl Write for Out {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        if self.ptr == 0 || self.len as usize + s.len() > 512 {
            return Err(core::fmt::Error);
        }
        unsafe {
            core::ptr::copy_nonoverlapping(
                s.as_ptr(),
                (self.ptr + self.len) as *mut u8,
                s.len(),
            );
        }
        self.len += s.len() as u32;
        Ok(())
    }
}

/// Realtime clock in nanoseconds, as `{"ok": <ns>}`; a WASI errno as
/// `{"ok": {"errno": <n>}}`.
#[no_mangle]
pub extern "C" fn clock_ns() -> i64 {
    let mut ns: u64 = 0;
    let errno = unsafe { clock_time_get(0, 1, &mut ns) };
    let mut out = Out::new();
    if errno == 0 {
        let _ = write!(out, "{{\"ok\":{ns}}}");
    } else {
        let _ = write!(out, "{{\"ok\":{{\"errno\":{errno}}}}}");
    }
    out.packed()
}

/// Eight random bytes as `{"ok": "<16 hex chars>"}`.
#[no_mangle]
pub extern "C" fn random_hex() -> i64 {
    let mut buf = [0u8; 8];
    let errno = unsafe { random_get(buf.as_mut_ptr(), buf.len()) };
    let mut out = Out::new();
    if errno == 0 {
        let _ = out.write_str("{\"ok\":\"");
        for b in buf {
            let _ = write!(out, "{b:02x}");
        }
        let _ = out.write_str("\"}");
    } else {
        let _ = write!(out, "{{\"ok\":{{\"errno\":{errno}}}}}");
    }
    out.packed()
}

/// Write one fixed line to `fd`, answering `{"ok": {"errno": <n>, "written": <n>}}`.
#[no_mangle]
pub extern "C" fn write_line(fd: i32) -> i64 {
    let line = b"probe says hello\n";
    let iov = Ciovec {
        buf: line.as_ptr(),
        len: line.len(),
    };
    let mut written: usize = 0;
    let errno = unsafe { fd_write(fd as u32, &iov, 1, &mut written) };
    let mut out = Out::new();
    let _ = write!(out, "{{\"ok\":{{\"errno\":{errno},\"written\":{written}}}}}");
    out.packed()
}
