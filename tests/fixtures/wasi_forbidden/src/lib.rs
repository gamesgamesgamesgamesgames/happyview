//! A plugin that links `path_open`. No capability covers a filesystem
//! import, so the loader refuses this module whatever its manifest declares.
#![no_std]

#[link(wasm_import_module = "wasi_snapshot_preview1")]
extern "C" {
    fn path_open(
        dirfd: u32,
        dirflags: u32,
        path: *const u8,
        path_len: usize,
        oflags: u16,
        fs_rights_base: u64,
        fs_rights_inheriting: u64,
        fdflags: u16,
        out: *mut u32,
    ) -> u16;
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}

#[no_mangle]
pub extern "C" fn alloc(_size: u32) -> u32 {
    0
}

#[no_mangle]
pub extern "C" fn dealloc(_ptr: u32, _len: u32) {}

#[no_mangle]
pub extern "C" fn open_root() -> i64 {
    let mut fd: u32 = 0;
    let errno = unsafe { path_open(3, 0, b".".as_ptr(), 1, 0, 0, 0, 0, &mut fd) };
    errno as i64
}
