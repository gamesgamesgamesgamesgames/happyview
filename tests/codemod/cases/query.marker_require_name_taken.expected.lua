function handle(input, ctx)
  local record = { title = "hi" }
  -- codemod: 'record' is bound by this script, so this use cannot be rewritten -- rename the binding, then rewrite this use by hand
  local a = Record.load(input.uri)
  -- codemod: 'record' is bound by this script, so this use cannot be rewritten -- rename the binding, then rewrite this use by hand
  local b = Record.load(input.other)
  return { record = record, a = a, b = b }
end
