function handle()
  if atproto.spaces.is_member(params.uri, caller_did) then
    return {
      access = atproto.spaces.get_access(params.uri, caller_did),
      members = atproto.spaces.list_members(params.uri),
    }
  end
  return {}
end
