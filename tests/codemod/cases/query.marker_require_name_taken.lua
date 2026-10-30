function handle()
  local record = { title = "hi" }
  local a = Record.load(params.uri)
  local b = Record.load(params.other)
  return { record = record, a = a, b = b }
end
