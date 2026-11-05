local record = require("happyview.record")
local atproto = require("happyview.atproto")

function handle(input, ctx)
  local dl = atproto.blob_download(input.did, input.cid)
  return record.upload_blob(dl.bytes, dl.mime_type)
end
