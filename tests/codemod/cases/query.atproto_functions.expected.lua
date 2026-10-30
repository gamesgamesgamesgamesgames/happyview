local atproto = require("happyview.atproto")

function handle(input, ctx)
  local pds = atproto.resolve_service_endpoint(ctx.caller_did)
  local labels = atproto.get_labels(input.uri)
  local sig = atproto.sign({ ok = true })
  return { pds = pds, labels = labels, ok = atproto.verify_signature({ ok = true }, sig, ctx.caller_did) }
end
