//! HappyView script handlers in Rust.
//!
//! A crate built with this for `wasm32-wasip1` is a *script module*: HappyView
//! runs it the way it runs an interpreter, with the trigger id in each run
//! choosing the handler.
//!
//! ```ignore
//! use happyview_script::{export, join, json, query, Ctx, Result, Value};
//!
//! #[query("at.happyproto.registry.getArtifact")]
//! async fn get_artifact(input: Value, ctx: Ctx) -> Result<Value> {
//!     let (a, b) = join!(
//!         ctx.call("happyview.record", "get", vec![input["uri"].clone()]),
//!         ctx.call("http", "get", vec![json!("https://example.com")]),
//!     );
//!     Ok(json!({ "record": a?, "page": b? }))
//! }
//!
//! export!(get_artifact);
//! ```
//!
//! Library calls are the only thing a handler can wait on. `join!` over
//! several starts all of them before waiting for any, so they overlap on the
//! host; CPU work never runs while the module waits.
//!
//! # Panics
//!
//! A module must panic by aborting. HappyView refuses at load any module that
//! defines or imports an exception tag, because a frame that catches its own
//! `throw` can erase the fuel it spent. Stable Rust on `wasm32-wasip1` already
//! aborts and emits no exception handling; keep it that way by stating it in
//! the module crate's profile, and never build it with `panic = "unwind"`:
//!
//! ```toml
//! [profile.release]
//! panic = "abort"
//! ```

mod ctx;
mod error;
mod executor;
mod output;

pub use ctx::{Ctx, LibraryCall};
pub use error::{Error, Result};
pub use output::{IntoOutput, Json, Output};

pub use futures_util::{join, try_join};
pub use happyview_plugin_sdk::wire::Level;
pub use happyview_plugin_sdk::{json, ExecuteContext, Map, ScriptKind, Value};
pub use serde_json;

/// The guest interface this crate builds modules against. HappyView refuses
/// a module whose `happyview.interface` section names one it does not speak.
pub const INTERFACE_VERSION: &str = "1";
