function handle()
  local pds = atproto.resolve_service_endpoint(caller_did)
  local labels = atproto.get_labels(params.uri)
  local sig = atproto.sign({ ok = true })
  return { pds = pds, labels = labels, ok = atproto.verify_signature({ ok = true }, sig, caller_did) }
end
