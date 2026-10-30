//! The structured values a plugin exchanges with the host: its identity, the
//! call envelope, the API surface a library advertises to scripts, and the
//! inputs and outputs of an auth plugin's exports.
//!
//! They are defined in [`crate::wire`], which the host imports too; this module
//! is the import path plugins use.

pub use crate::wire::DEFAULT_CHUNK_NAME;
pub use crate::wire::{
    ApiExport, ApiMethod, ApiSurface, AtprotoBlobDownload, AtprotoResolveService, AttestSign,
    AttestVerify, AuthorizeUrlInput, BacklinksQuery, BlobData, Bytes, CallContext, CallInput,
    CallbackInput, CallerAccount, CallerBlobUpload, CallerEmail, CallerRecordCreate, CallerRecordDelete,
    CallerRecordPut, CallerXrpcProcedure, CallerXrpcQuery, ChunkRole, Condition, ExecuteContext,
    ExecuteInput, ExecuteLimits, ExecuteOutput, ExternalProfile, Filter, ImportRef, IndexDelete,
    IndexPut, JobCreate, JobGet, JobListAny, JobProgressRequest, JobShouldStopRequest, JobView,
    JobWaitRequest, Label, LabelsGet, LexiconGet, LibraryRef, LinkedRepoBlobUpload, LinkedRepoCall,
    LinkedRepoInfo, LinkedRepoRecordCreate, LinkedRepoRecordDelete, LinkedRepoRecordPut,
    MethodCall, ModuleResolve, ObjectCall, PanicReport, Patch, PluginInfo, PreparedArtifact,
    RecordEnvelope, RecordRef, RecordsCount, RecordsPage, RecordsQuery, RecordsSearch,
    RefreshInput, ResolvedModule, ScriptErrorKind, ScriptJob, ScriptKind, ScriptLogRequest,
    ScriptSpace, ScriptValueKind, Sort, SpaceDelete, SpaceInfo, SpaceInviteCreate, SpaceInviteInfo,
    SpaceMemberAdd, SpaceMemberInfo, SpaceMemberRemove, SpaceRecordDelete, SpaceRecordInfo,
    SpaceRecordPut, SpaceRecordWrite, SpaceRecordsPage, SpaceUpdate, SpacesAcceptInvite,
    SpacesAccess, SpacesCreate, SpacesInfo, SpacesMembers, SpacesQuery, Step, StrongRef,
    TableQuery, TestCall, TestCalls, TestMock, TestPdsWrite, TestPdsWritesRequest, TestReport,
    TestStatus, TestTrigger, TestTriggerKind, TestTriggerResult, TokenInput, TokenSet,
    ValidateError, ValidateInput, ValidateOutput,
};
