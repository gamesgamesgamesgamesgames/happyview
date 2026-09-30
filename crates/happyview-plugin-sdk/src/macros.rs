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
/// Use it directly only for a plugin that uses neither [`library_plugin!`] nor
/// [`auth_plugin!`], both of which emit it already. Exactly one call per crate.
///
/// The `alloc = system` form emits no allocator of its own: `alloc`/`dealloc`
/// go to whichever global allocator the plugin crate links, and `dealloc`
/// really frees. That is what an interpreter needs — it allocates and frees for
/// the whole run, which a heap that never reuses memory cannot serve — and it
/// requires the crate to bring an allocator, as a `std` target does.
///
/// It emits a `#[panic_handler]` everywhere but wasi, which is a proxy for
/// "this crate does not link `std`" and not the same thing: a `std` plugin on
/// `wasm32-unknown-unknown` gets two handlers and will not compile. There is
/// no cfg for what actually matters, so such a plugin drops `alloc = system`
/// and writes its own `alloc`/`dealloc` over
/// [`alloc_bytes`](crate::abi::alloc_bytes) and
/// [`free_bytes`](crate::abi::free_bytes). The interpreter this form exists
/// for targets wasip1.
///
/// ```ignore
/// happyview_plugin_sdk::export_abi!();            // 512 KiB heap
/// happyview_plugin_sdk::export_abi!(heap = 1 << 20);
/// happyview_plugin_sdk::export_abi!(alloc = system);
/// ```
#[macro_export]
macro_rules! export_abi {
    () => {
        $crate::export_abi!(heap = $crate::abi::DEFAULT_HEAP_SIZE);
    };
    (alloc = system) => {
        /// Trap rather than spin: a panic that looped here would burn the
        /// host's fuel budget instead of surfacing as an execution error.
        #[cfg(all(target_arch = "wasm32", not(target_os = "wasi")))]
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
            $crate::abi::free_bytes(ptr, size)
        }
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

/// Emit every export an auth plugin needs: `alloc`, `dealloc`, `plugin_info`,
/// `get_authorize_url`, `handle_callback`, `refresh_tokens` and `get_profile`,
/// plus the allocator and panic handler.
///
/// Each export decodes its own input struct and wraps whatever the handler
/// returns. Input it cannot parse becomes a `BAD_INPUT` error envelope — it
/// never panics and never traps.
///
/// An auth plugin's `info` should set
/// [`auth_type`](crate::PluginInfo::auth_type) — `"oauth2"`, `"openid"` or
/// `"api_key"` — and list the env-var names it needs in
/// [`required_secrets`](crate::PluginInfo::required_secrets).
///
/// ```ignore
/// happyview_plugin_sdk::auth_plugin! {
///     info: PluginInfo::new("auth-steam", "Steam", "1.0.0")
///         .auth_type("openid")
///         .required_secrets(["PLUGIN_AUTH_STEAM_API_KEY"]),
///     authorize_url: authorize_url,
///     callback: callback,
///     refresh: refresh,
///     profile: profile,
/// }
///
/// fn authorize_url(input: &AuthorizeUrlInput) -> Result<String, PluginError> { todo!() }
/// fn callback(input: &CallbackInput) -> Result<TokenSet, PluginError> { todo!() }
/// fn refresh(input: &RefreshInput) -> Result<TokenSet, PluginError> { todo!() }
/// fn profile(input: &TokenInput) -> Result<ExternalProfile, PluginError> { todo!() }
/// ```
///
/// Pass `heap = <bytes>` as the first field to size the guest heap.
#[macro_export]
macro_rules! auth_plugin {
    (
        info: $info:expr,
        authorize_url: $authorize_url:expr,
        callback: $callback:expr,
        refresh: $refresh:expr,
        profile: $profile:expr $(,)?
    ) => {
        $crate::auth_plugin! {
            heap = $crate::abi::DEFAULT_HEAP_SIZE,
            info: $info,
            authorize_url: $authorize_url,
            callback: $callback,
            refresh: $refresh,
            profile: $profile,
        }
    };
    (
        heap = $heap:expr,
        info: $info:expr,
        authorize_url: $authorize_url:expr,
        callback: $callback:expr,
        refresh: $refresh:expr,
        profile: $profile:expr $(,)?
    ) => {
        $crate::export_abi!(heap = $heap);

        #[cfg_attr(target_arch = "wasm32", no_mangle)]
        pub extern "C" fn plugin_info() -> i64 {
            let info: $crate::PluginInfo = $info;
            $crate::abi::return_ok(&info)
        }

        #[cfg_attr(target_arch = "wasm32", no_mangle)]
        pub extern "C" fn get_authorize_url(ptr: u32, len: u32) -> i64 {
            let handler: fn(
                &$crate::AuthorizeUrlInput,
            ) -> core::result::Result<
                $crate::__private::String,
                $crate::PluginError,
            > = $authorize_url;
            $crate::abi::dispatch_input(ptr, len, handler)
        }

        #[cfg_attr(target_arch = "wasm32", no_mangle)]
        pub extern "C" fn handle_callback(ptr: u32, len: u32) -> i64 {
            let handler: fn(
                &$crate::CallbackInput,
            ) -> core::result::Result<$crate::TokenSet, $crate::PluginError> = $callback;
            $crate::abi::dispatch_input(ptr, len, handler)
        }

        #[cfg_attr(target_arch = "wasm32", no_mangle)]
        pub extern "C" fn refresh_tokens(ptr: u32, len: u32) -> i64 {
            let handler: fn(
                &$crate::RefreshInput,
            ) -> core::result::Result<$crate::TokenSet, $crate::PluginError> = $refresh;
            $crate::abi::dispatch_input(ptr, len, handler)
        }

        #[cfg_attr(target_arch = "wasm32", no_mangle)]
        pub extern "C" fn get_profile(ptr: u32, len: u32) -> i64 {
            let handler: fn(
                &$crate::TokenInput,
            )
                -> core::result::Result<$crate::ExternalProfile, $crate::PluginError> = $profile;
            $crate::abi::dispatch_input(ptr, len, handler)
        }
    };
}

/// Emit every export an interpreter plugin needs: `alloc`, `dealloc`,
/// `plugin_info`, `execute` and `validate`.
///
/// Each input-taking export decodes its own struct and wraps whatever the
/// handler returns. Input it cannot parse becomes a `BAD_INPUT` error
/// envelope — it never panics and never traps. A script that failed is an
/// `Ok(ExecuteOutput::Error { .. })`, not a `PluginError`: the envelope's error
/// half is for the interpreter itself failing.
///
/// The allocator is the crate's own, through
/// [`export_abi!(alloc = system)`](export_abi) — no heap of a fixed size
/// survives an interpreter — so an interpreter plugin links `std` or declares
/// a `#[global_allocator]`.
///
/// ```ignore
/// happyview_plugin_sdk::interpreter_plugin! {
///     info: PluginInfo::new("lua", "Lua", "1.0.0"),
///     execute: execute,
///     validate: validate,
/// }
///
/// fn execute(input: &ExecuteInput) -> Result<ExecuteOutput, PluginError> { todo!() }
/// fn validate(input: &ValidateInput) -> Result<ValidateOutput, PluginError> { todo!() }
/// ```
#[macro_export]
macro_rules! interpreter_plugin {
    (info: $info:expr, execute: $execute:expr, validate: $validate:expr $(,)?) => {
        $crate::export_abi!(alloc = system);

        #[cfg_attr(target_arch = "wasm32", no_mangle)]
        pub extern "C" fn plugin_info() -> i64 {
            let info: $crate::PluginInfo = $info;
            $crate::abi::return_ok(&info)
        }

        #[cfg_attr(target_arch = "wasm32", no_mangle)]
        pub extern "C" fn execute(ptr: u32, len: u32) -> i64 {
            let handler: fn(
                &$crate::ExecuteInput,
            )
                -> core::result::Result<$crate::ExecuteOutput, $crate::PluginError> = $execute;
            $crate::abi::dispatch_execute(ptr, len, handler)
        }

        #[cfg_attr(target_arch = "wasm32", no_mangle)]
        pub extern "C" fn validate(ptr: u32, len: u32) -> i64 {
            let handler: fn(
                &$crate::ValidateInput,
            )
                -> core::result::Result<$crate::ValidateOutput, $crate::PluginError> = $validate;
            $crate::abi::dispatch_validate(ptr, len, handler)
        }
    };
}

/// The macro's items, expanded in their own module so the export names cannot
/// collide with anything else here. Off wasm32 they carry no `no_mangle` and
/// linear memory is unavailable, so what this pins is that the expansion
/// compiles against the handler signatures and answers when called.
#[cfg(test)]
mod interpreter_macro {
    use crate::{
        ExecuteInput, ExecuteOutput, PluginError, PluginInfo, ScriptValueKind, ValidateInput,
        ValidateOutput,
    };

    crate::interpreter_plugin! {
        info: PluginInfo::new("fixture", "Fixture", "1.0.0"),
        execute: execute_script,
        validate: validate_script,
    }

    fn execute_script(input: &ExecuteInput) -> Result<ExecuteOutput, PluginError> {
        Ok(ExecuteOutput::Returned {
            value: serde_json::Value::String(input.source.clone()),
            value_kind: ScriptValueKind::Other,
        })
    }

    fn validate_script(_input: &ValidateInput) -> Result<ValidateOutput, PluginError> {
        Ok(ValidateOutput::valid())
    }

    #[test]
    fn the_macro_emits_the_five_items_the_host_resolves() {
        assert_eq!(alloc(16), 0);
        dealloc(0, 0);
        assert_eq!(plugin_info(), 0);
        assert_eq!(execute(0, 0), 0);
        assert_eq!(validate(0, 0), 0);
    }
}
