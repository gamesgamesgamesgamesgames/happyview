function handle(input, ctx)
  -- codemod: TID.toISO8601 has no mechanical equivalent -- rewrite it by hand
  local at = TID.toISO8601(input.rkey)
  -- codemod: TID.toNumber has no mechanical equivalent -- rewrite it by hand
  return { at = at, n = TID.toNumber(input.rkey) }
end
