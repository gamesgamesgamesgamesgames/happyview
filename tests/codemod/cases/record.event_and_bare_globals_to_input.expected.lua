function handle(input, ctx)
  if input.action == "delete" then
    return nil
  end
  return {
    uri = input.uri,
    did = input.did,
    collection = input.collection,
    rkey = input.rkey,
    title = input.record.title,
    raw = input,
  }
end
