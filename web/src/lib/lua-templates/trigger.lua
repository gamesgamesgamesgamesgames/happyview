-- Trigger script: `input` is the trigger's payload; `ctx` describes
-- this invocation (caller, environment, trigger id, and more). Return
-- a transformed value, or `nil` to skip the operation.
--
-- require("internal.*") and installed libraries provide the rest.

local log = require("internal.logging")

function handle(input, ctx)
  log.info("script fired", { trigger = ctx.trigger, caller = ctx.caller_did })
  return input
end
