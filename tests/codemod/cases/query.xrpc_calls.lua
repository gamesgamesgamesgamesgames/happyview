function handle()
  local resp = xrpc.query("app.example.list", { limit = 5 })
  if resp.status ~= 200 then
    return { error = resp.body }
  end
  return xrpc.procedure("app.example.set", { status = "hi" })
end
