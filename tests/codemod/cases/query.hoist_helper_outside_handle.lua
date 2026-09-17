local function audit(action)
  log(caller_did .. " did " .. action)
  return env.AUDIT_SINK
end

function handle()
  audit("list")
  return { q = params.q }
end
