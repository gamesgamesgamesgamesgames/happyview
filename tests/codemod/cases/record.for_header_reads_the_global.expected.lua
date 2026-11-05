function handle(input, ctx)
  -- codemod: 'record' is bound by this script, so this use cannot be rewritten -- rename the binding, then rewrite this use by hand
  for _, record in ipairs(record.items) do end
end
