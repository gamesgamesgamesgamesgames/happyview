---@meta happyview.jobs

--- Enqueue background jobs for the job worker
---@class happyview.jobs
local M = {}

--- Enqueue a job; returns its id
---@param job_type string Free-form job type name
---@param input? table<string, any>|nil Job input; nil sends {}
---@param opts? { auth?: boolean|nil }|nil Job options
---@return string
function M.create(job_type, input, opts) end

--- Read one of the caller's own jobs; nil if it is not theirs or absent
---@param id string The job's id
---@return table<string, any>|nil
function M.get(id) end

--- Read any user's job; nil if absent
---@param id string The job's id
---@return table<string, any>|nil
function M.get_any(id) end

--- List jobs across every user, newest first
---@param opts? { status?: any[]|nil, job_type?: string|nil, limit?: integer|nil }|nil List filters
---@return any[]
function M.list_any(opts) end

return M
