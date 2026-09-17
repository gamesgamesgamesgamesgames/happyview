function handle()
  local dl = atproto.blob_download(input.did, input.cid)
  return atproto.blob_upload(dl.handle, dl.mimeType)
end
