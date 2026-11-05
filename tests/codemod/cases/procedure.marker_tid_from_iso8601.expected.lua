function handle(input, ctx)
  -- codemod: TID.fromISO8601 has no mechanical equivalent -- rewrite it by hand
  return { rkey = TID.fromISO8601(ctx.params.at) }
end
