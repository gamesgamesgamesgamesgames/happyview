function handle()
  local s = atproto.spaces.get(params.uri)
  local repo = linked_repos.get(params.did)
  if not s then
    return { error = "no such space" }
  end
  if repo == nil then
    return { error = "not linked" }
  end
  return { ok = true }
end
