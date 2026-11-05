function handle(input, ctx)
  local keys = {}
  for k in pairs(ctx) do keys[#keys + 1] = k end
  table.sort(keys)
  return { keys = keys, trigger = ctx.trigger, caller = ctx.caller_did, auth = ctx.has_pds_auth, env = ctx.env.API_KEY, env_missing = type(ctx.env.NOPE), q = ctx.params.q, limit_type = math.type(ctx.params.limit), job = type(ctx.job), space = type(ctx.space) }
end
