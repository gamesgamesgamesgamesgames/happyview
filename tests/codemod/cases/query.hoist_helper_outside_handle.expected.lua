local log = require("internal.logging")

local input, ctx

local function audit(action)
  log.info(ctx.caller_did .. " did " .. action)
  return ctx.env.AUDIT_SINK
end

function handle(handle_input, handle_ctx)
  input, ctx = handle_input, handle_ctx
  audit("list")
  return { q = input.q }
end
