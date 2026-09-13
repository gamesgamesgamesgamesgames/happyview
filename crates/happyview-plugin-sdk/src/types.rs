//! The structured values a plugin exchanges with the host: its identity, the
//! call envelope, the API surface a library advertises to scripts, and the
//! inputs and outputs of an auth plugin's exports.
//!
//! They are defined in [`crate::wire`], which the host imports too; this module
//! is the import path plugins use.

pub use crate::wire::{
    ApiExport, ApiMethod, ApiSurface, AuthorizeUrlInput, BacklinksQuery, CallContext, CallInput,
    CallbackInput, Condition, ExternalProfile, Filter, MethodCall, ObjectCall, PluginInfo,
    RecordsCount, RecordsPage, RecordsQuery, RecordsSearch, RefreshInput, Sort, Step, StrongRef,
    TableQuery, TokenInput, TokenSet,
};
