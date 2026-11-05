function handle()
  if action == "delete" then
    return nil
  end
  return {
    uri = uri,
    did = did,
    collection = collection,
    rkey = rkey,
    title = record.title,
    raw = event,
  }
end
