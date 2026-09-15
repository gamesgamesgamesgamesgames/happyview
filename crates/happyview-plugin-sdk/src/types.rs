//! The structured values a plugin exchanges with the host: its identity, the
//! call envelope, the API surface a library advertises to scripts, and the
//! inputs and outputs of an auth plugin's exports.
//!
//! They are defined in [`crate::wire`], which the host imports too; this module
//! is the import path plugins use.

pub use crate::wire::{
    ApiExport, ApiMethod, ApiSurface, AtprotoBlobDownload, AtprotoResolveService, AttestSign,
    AttestVerify, AuthorizeUrlInput, BacklinksQuery, BlobData, CallContext, CallInput,
    CallbackInput, CallerBlobUpload, CallerRecordCreate, CallerRecordDelete, CallerRecordPut,
    CallerXrpcProcedure, CallerXrpcQuery, Condition, ExternalProfile, Filter, IndexDelete,
    IndexPut, JobCreate, Label, LabelsGet, LexiconGet, LinkedRepoBlobUpload, LinkedRepoCall,
    LinkedRepoInfo, LinkedRepoRecordCreate, LinkedRepoRecordDelete, LinkedRepoRecordPut,
    MethodCall, ObjectCall, PluginInfo, RecordRef, RecordsCount, RecordsPage, RecordsQuery,
    RecordsSearch, RefreshInput, Sort, Step, StrongRef, TableQuery, TokenInput, TokenSet,
};
