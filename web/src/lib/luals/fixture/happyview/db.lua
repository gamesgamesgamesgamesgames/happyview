---@meta happyview.db

--- An indexed record beside where it came from
---@class happyview.db.RecordEnvelope
---@field uri string The record's AT URI
---@field did string The repo it is in
---@field collection string Its collection NSID
---@field rkey string Its record key
---@field cid? string|nil Its CID; null for a record written locally and not yet seen on the network
---@field indexed_at? string|nil When the network's copy was indexed; null until it has been
---@field record table<string, any> The record body

--- One page of a query
---@class happyview.db.RecordsPage
---@field records happyview.db.RecordEnvelope[]
---@field cursor? string|nil Pass to cursor() for the next page; absent on the last

--- A comparison where() accepts
---@alias happyview.db.Operator "="|"!="|"<"|">"|"<="|">="|"like"|"not like"|"ilike"

--- Query indexed records
---@class happyview.db
local M = {}

---@class happyview.db.records.Object
local records_object = {}

--- Keep records whose field compares true; every where() must hold
---@param field? string|nil Record field path; nil skips the step
---@param op happyview.db.Operator How to compare
---@param value string|number|boolean What to compare with
---@return happyview.db.records.Object
function records_object:where(field, op, value) end

--- Order by a field
---@param field? string|nil Record field path; nil skips the step
---@param direction? "asc"|"desc"|nil Default desc
---@return happyview.db.records.Object
function records_object:sort(field, direction) end

--- At most this many records
---@param n? integer|nil nil skips the step
---@return happyview.db.records.Object
function records_object:limit(n) end

--- Continue from a page's cursor
---@param cursor? string|nil nil skips the step
---@return happyview.db.records.Object
function records_object:cursor(cursor) end

--- Only records in one repo
---@param did? string|nil nil skips the step
---@return happyview.db.records.Object
function records_object:did(did) end

--- Run the query
---@return happyview.db.RecordsPage
function records_object:run() end

--- Count what the query matches
---@return integer
function records_object:count() end

--- The first record the query matches, or null
---@return happyview.db.RecordEnvelope|nil
function records_object:first() end

--- Start a query over one collection
---@param collection string Lexicon collection NSID
---@return happyview.db.records.Object
function M.records(collection) end

--- One record by AT URI, or null
---@param uri string AT URI
---@return happyview.db.RecordEnvelope|nil
function M.get(uri) end

--- Substring search on one field, ranked by match position
---@param collection string Lexicon collection NSID
---@param field string Record field path
---@param query string Text to find
---@param limit? integer|nil Max results, default 10, max 100
---@return happyview.db.RecordEnvelope[]
function M.search(collection, field, query, limit) end

--- Which database the instance runs on
---@return "sqlite"|"postgres"|nil
function M.backend() end

return M
