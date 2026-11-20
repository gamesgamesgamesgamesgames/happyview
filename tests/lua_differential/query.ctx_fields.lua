function handle(input, ctx)
  local keys = {}
  -- ctx.caller_assertion is plugin-only (the host_caller_assertion import);
  -- the native runner has no such function. Leave it out so every other key
  -- and value is still compared.
  for k in pairs(ctx) do
    if k ~= "caller_assertion" then keys[#keys + 1] = k end
  end
  table.sort(keys)
  return { keys = keys, trigger = ctx.trigger, caller = ctx.caller_did, auth = ctx.has_pds_auth, env = ctx.env.API_KEY, env_missing = type(ctx.env.NOPE), q = ctx.params.q, limit_type = math.type(ctx.params.limit), job = type(ctx.job), space = type(ctx.space) }
end
