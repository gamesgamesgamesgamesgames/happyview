//! The macros that put a plugin's wasm exports in the plugin's own crate.
//!
//! They have to be macros rather than plain SDK items: a `#[no_mangle]` symbol
//! defined in a dependency rlib is not reliably kept as a wasm export by the
//! linker, and a `no_std` cdylib needs its one `#[panic_handler]` in the final
//! crate. Emitting both here means every plugin gets them, in the right crate,
//! without hand-rolling any of the ABI.

/// Emit the guest allocator and the `alloc`/`dealloc` exports the host calls
/// to place data in this module's memory, plus the wasm `#[panic_handler]`.
///
/// Use it directly only for a plugin that does not use [`library_plugin!`],
/// which emits it already. Exactly one call per crate.
///
/// ```ignore
/// happyview_plugin_sdk::export_abi!();            // 512 KiB heap
/// happyview_plugin_sdk::export_abi!(heap = 1 << 20);
/// ```
#[macro_export]
macro_rules! export_abi {
    () => {
        $crate::export_abi!(heap = $crate::abi::DEFAULT_HEAP_SIZE);
    };
    (heap = $heap:expr) => {
        #[cfg(target_arch = "wasm32")]
        const __HV_HEAP_SIZE: usize = $heap;
        #[cfg(target_arch = "wasm32")]
        static mut __HV_HEAP: [u8; __HV_HEAP_SIZE] = [0; __HV_HEAP_SIZE];
        #[cfg(target_arch = "wasm32")]
        static mut __HV_HEAP_POS: usize = 0;

        #[cfg(target_arch = "wasm32")]
        struct __HvBumpAllocator;

        // SAFETY: the heap is a single static buffer owned by this allocator,
        // and `bump_alloc` never hands out overlapping regions.
        #[cfg(target_arch = "wasm32")]
        unsafe impl core::alloc::GlobalAlloc for __HvBumpAllocator {
            unsafe fn alloc(&self, layout: core::alloc::Layout) -> *mut u8 {
                $crate::abi::bump_alloc(
                    &raw mut __HV_HEAP as *mut u8,
                    __HV_HEAP_SIZE,
                    &raw mut __HV_HEAP_POS,
                    layout,
                )
            }

            /// A plugin instance is torn down after each call, so memory is
            /// reclaimed by dropping the whole instance, never here.
            unsafe fn dealloc(&self, _ptr: *mut u8, _layout: core::alloc::Layout) {}
        }

        #[cfg(target_arch = "wasm32")]
        #[global_allocator]
        static __HV_ALLOCATOR: __HvBumpAllocator = __HvBumpAllocator;

        /// Trap rather than spin: a panic that looped here would burn the
        /// host's fuel budget instead of surfacing as an execution error.
        #[cfg(target_arch = "wasm32")]
        #[panic_handler]
        fn __hv_panic(_info: &core::panic::PanicInfo) -> ! {
            core::arch::wasm32::unreachable()
        }

        #[cfg_attr(target_arch = "wasm32", no_mangle)]
        pub extern "C" fn alloc(size: u32) -> u32 {
            $crate::abi::alloc_bytes(size)
        }

        #[cfg_attr(target_arch = "wasm32", no_mangle)]
        pub extern "C" fn dealloc(ptr: u32, size: u32) {
            $crate::abi::dealloc_bytes(ptr, size)
        }
    };
}

/// Emit every export a library plugin needs: `alloc`, `dealloc`, `plugin_info`,
/// `get_api_surface` and `call`, plus the allocator and panic handler.
///
/// `call` decodes the host's envelope, hands the function name, arguments and
/// context to the handler, and wraps whatever comes back. Input it cannot parse
/// becomes a `BAD_INPUT` error envelope — it never panics and never traps.
///
/// ```ignore
/// happyview_plugin_sdk::library_plugin! {
///     info: PluginInfo::new("http", "HTTP Client", "1.0.0"),
///     surface: surface,
///     call: dispatch,
/// }
///
/// fn surface() -> ApiSurface { ApiSurface::new("http") }
///
/// fn dispatch(function: &str, args: &[Value], ctx: &CallContext)
///     -> Result<Value, PluginError> { todo!() }
/// ```
///
/// Pass `heap = <bytes>` as the first field to size the guest heap.
#[macro_export]
macro_rules! library_plugin {
    (info: $info:expr, surface: $surface:expr, call: $call:expr $(,)?) => {
        $crate::library_plugin! {
            heap = $crate::abi::DEFAULT_HEAP_SIZE,
            info: $info,
            surface: $surface,
            call: $call,
        }
    };
    (heap = $heap:expr, info: $info:expr, surface: $surface:expr, call: $call:expr $(,)?) => {
        $crate::export_abi!(heap = $heap);

        #[cfg_attr(target_arch = "wasm32", no_mangle)]
        pub extern "C" fn plugin_info() -> i64 {
            let info: $crate::PluginInfo = $info;
            $crate::abi::return_ok(&info)
        }

        #[cfg_attr(target_arch = "wasm32", no_mangle)]
        pub extern "C" fn get_api_surface() -> i64 {
            let surface: fn() -> $crate::ApiSurface = $surface;
            $crate::abi::return_ok(&surface())
        }

        #[cfg_attr(target_arch = "wasm32", no_mangle)]
        pub extern "C" fn call(ptr: u32, len: u32) -> i64 {
            let handler: fn(
                &str,
                &[$crate::Value],
                &$crate::CallContext,
            ) -> core::result::Result<$crate::Value, $crate::PluginError> = $call;
            $crate::abi::dispatch_call(ptr, len, handler)
        }
    };
}
