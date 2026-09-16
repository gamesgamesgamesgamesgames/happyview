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
  return `local log = require("internal.logging")

function handle(input, ctx)
  local r = Record("${target}", input)
  r:save()
  log.info("record saved", { uri = r._uri })
  return { uri = r._uri, cid = r._cid }
end
`;
}

export function indexHookScript(): string {
  return `local log = require("internal.logging")

function handle(input, ctx)
  if input.action == "delete" then
    -- record was deleted
    log.info("deleted " .. input.uri)
  else
    -- record was created or updated
    log.info(input.action .. " " .. input.uri)
  end
end
`;
}

export function queryScript(): string {
  return `local log = require("internal.logging")

function handle(input, ctx)
  log.info("handling query", { uri = input.uri })

  if input.uri then
    local record = db.get(input.uri)
    if not record then
      error("record not found")
    end
    return { record = record }
  end

  return db.query({
    collection = ctx.collection,
    did = input.did,
    limit = input.limit,
    cursor = input.cursor,
  })
end
`;
}
