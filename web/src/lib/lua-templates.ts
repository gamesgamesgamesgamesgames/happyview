export const LEXICON_TEMPLATE = JSON.stringify(
  {
    $type: "com.atproto.lexicon.schema",
    lexicon: 1,
    id: "com.example.myRecord",
    defs: {
      main: {
        type: "record",
        key: "tid",
        record: {
          type: "object",
          required: [],
          properties: {},
        },
      },
    },
  },
  null,
  2,
);

export function procedureScript(collection: string): string {
  const target = collection || "COLLECTION";
  return `function handle()
  local r = Record("${target}", input)
  r:save()
  return { uri = r._uri, cid = r._cid }
end
`;
}

export function indexHookScript(): string {
  return `function handle()
  if action == "delete" then
    -- record was deleted
    log("deleted " .. uri)
  else
    -- record was created or updated
    log(action .. " " .. uri)
  end
end
`;
}

export function queryScript(collection: string): string {
  const target = collection || "COLLECTION";
  return `collection = "${target}"

function handle()
  if params.uri then
    local record = db.get(params.uri)
    if not record then
      error("record not found")
    end
    return { record = record }
  end

  return db.query({
    collection = collection,
    did = params.did,
    limit = params.limit,
    cursor = params.cursor,
  })
end
`;
}
