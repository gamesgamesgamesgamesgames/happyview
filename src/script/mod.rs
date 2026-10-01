//! The one path from a stored script to the interpreter that runs it.
//!
//! Every runner — XRPC query and procedure, record event, label, job — reaches
//! an interpreter through [`dispatch`], and the input it sends is built by
//! [`input::build_input`]. One builder rather than one per runner, because the
//! wire struct has a field for every kind and a call site that assembles it
//! itself is a call site that can forget one.

pub mod dispatch;
pub mod input;

pub use dispatch::{DispatchError, dispatch, no_interpreter_message};
pub use input::{Invocation, Trigger, build_input};
