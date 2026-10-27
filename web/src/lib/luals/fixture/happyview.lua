---@meta

--- ctx for an XRPC query
---@class happyview.QueryCtx
---@field trigger string The trigger that ran this script
---@field has_pds_auth boolean Whether library calls can act as the caller on their PDS
---@field env table<string, string> The script variables
---@field caller_did? string The caller's DID
---@field method string The XRPC method's NSID
---@field collection string The record collection this method operates on
---@field space? { uri: string, id: string, did: string, authority_did: string, spaceType: string, skey: string } The space the request is scoped to

--- ctx for an XRPC procedure
---@class happyview.ProcedureCtx
---@field trigger string The trigger that ran this script
---@field has_pds_auth boolean Whether library calls can act as the caller on their PDS
---@field env table<string, string> The script variables
---@field caller_did string The caller's DID
---@field method string The XRPC method's NSID
---@field collection string The record collection this method operates on
---@field params table<string, any> The query parameters the request carried
---@field delegate_did? string|nil The account the caller is acting for, when it delegated
---@field space? { uri: string, id: string, did: string, authority_did: string, spaceType: string, skey: string } The space the request is scoped to

--- ctx for a record-event script
---@class happyview.RecordCtx
---@field trigger string The trigger that ran this script
---@field has_pds_auth boolean Whether library calls can act as the caller on their PDS
---@field env table<string, string> The script variables
---@field caller_did string The record's author
---@field collection string The record collection this method operates on

--- ctx for a label script
---@class happyview.LabelCtx
---@field trigger string The trigger that ran this script
---@field has_pds_auth boolean Whether library calls can act as the caller on their PDS
---@field env table<string, string> The script variables

--- ctx for a job
---@class happyview.JobCtx
---@field trigger string The trigger that ran this script
---@field has_pds_auth boolean Whether library calls can act as the caller on their PDS
---@field env table<string, string> The script variables
---@field caller_did? string Who created the job, when anyone did
---@field job happyview.Job The job this run is

--- ctx for a test file
---@class happyview.TestCtx
---@field trigger string The trigger that ran this script
---@field has_pds_auth boolean Whether library calls can act as the caller on their PDS
---@field env table<string, string> The script variables

--- The job this run is, and its controls
---@class happyview.Job
---@field id string The job's id
local Job = {}

--- Save progress, which the dashboard shows
---@param data table
function Job.progress(data) end

--- Whether the job has been paused or cancelled; stop when it has
---@return boolean
function Job.should_stop() end

--- Sleep, from 0 to 3600 seconds
---@param seconds number
function Job.wait(seconds) end

--- What a record-event script receives
---@class happyview.RecordEvent
---@field action "create"|"update"|"delete"
---@field uri string The record's AT URI
---@field did string The repo the record is in
---@field collection string The record's collection NSID
---@field rkey string The record's key
---@field record any The record body; null for a delete

--- What a label script receives
---@class happyview.LabelEvent
---@field src string The labeler's DID
---@field uri string What the label applies to
---@field val string The label's value
---@field neg boolean Whether this removes the label
---@field cts string When the label was created
---@field exp? string|nil When the label expires

--- A blob, as a record holds one
---@class happyview.Blob
---@field ["$type"] "blob"
---@field ref happyview.CidLink
---@field mimeType string
---@field size integer

--- A CID link, as a record holds one
---@class happyview.CidLink
---@field ["$link"] string

--- Bytes, as a record holds them
---@class happyview.Bytes
---@field ["$bytes"] string The bytes, base64
