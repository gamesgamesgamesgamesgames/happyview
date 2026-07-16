export interface LuaCompletionEntry {
  label: string;
  detail?: string;
  description?: string;
  insertText?: string;
}

export interface LuaSnippetEntry {
  label: string;
  insertText: string;
  detail: string;
  description?: string;
}

export const LUA_KEYWORDS = [
  "and", "break", "do", "else", "elseif", "end", "false",
  "for", "function", "goto", "if", "in", "local", "nil",
  "not", "or", "repeat", "return", "then", "true", "until", "while",
];

export const LUA_BUILTINS = [
  "print", "tostring", "tonumber", "type", "pairs", "ipairs", "next",
  "select", "unpack", "error", "pcall", "xpcall", "assert",
  "setmetatable", "getmetatable", "rawget", "rawset", "rawequal",
  // Standard library modules
  "string", "table", "math", "coroutine", "utf8",
  // HappyView sandbox globals
  "input", "params", "caller_did", "collection", "method",
  "now", "log", "TID", "toarray", "env",
  // Hook-specific globals
  "action", "uri", "did", "rkey", "record",
  // HappyView API modules
  "http", "xrpc", "atproto",
];

export const LUA_SNIPPETS: LuaSnippetEntry[] = [
  {
    label: "if",
    insertText: "if ${1:condition} then\n\t$0\nend",
    detail: "if ... then ... end",
  },
  {
    label: "if",
    insertText: "if ${1:condition} then\n\t$2\nelse\n\t$0\nend",
    detail: "if ... then ... else ... end",
  },
  {
    label: "if",
    insertText: "if ${1:condition} then\n\t$2\nelseif ${3:condition} then\n\t$0\nend",
    detail: "if ... then ... elseif ... end",
  },
  {
    label: "elseif",
    insertText: "elseif ${1:condition} then\n\t$0",
    detail: "elseif ... then",
  },
  {
    label: "for",
    insertText: "for ${1:i} = ${2:1}, ${3:10} do\n\t$0\nend",
    detail: "for i = start, stop do ... end",
  },
  {
    label: "for",
    insertText: "for ${1:i}, ${2:v} in ipairs(${3:t}) do\n\t$0\nend",
    detail: "for i, v in ipairs(t) do ... end",
  },
  {
    label: "for",
    insertText: "for ${1:k}, ${2:v} in pairs(${3:t}) do\n\t$0\nend",
    detail: "for k, v in pairs(t) do ... end",
  },
  {
    label: "while",
    insertText: "while ${1:condition} do\n\t$0\nend",
    detail: "while ... do ... end",
  },
  {
    label: "repeat",
    insertText: "repeat\n\t$0\nuntil ${1:condition}",
    detail: "repeat ... until ...",
  },
  {
    label: "function",
    insertText: "function ${1:name}(${2:})\n\t$0\nend",
    detail: "function name(...) ... end",
  },
  {
    label: "function",
    insertText: "local function ${1:name}(${2:})\n\t$0\nend",
    detail: "local function name(...) ... end",
  },
  {
    label: "local",
    insertText: "local ${1:name} = ${0}",
    detail: "local name = ...",
  },
  {
    label: "return",
    insertText: "return ${0}",
    detail: "return ...",
  },
];

export type LuaCompletions = Record<string, LuaCompletionEntry[]>;

/** Map of collection NSID → record property completions */
export type CollectionSchemas = Record<string, LuaCompletionEntry[]>;

const STATIC_COMPLETIONS: LuaCompletions = {
  Record: [
    { label: "save_all", detail: "function", description: "Save multiple records in parallel — Record.save_all({ r1, r2 })" },
    { label: "load", detail: "function", description: "Load a record from the database by AT URI — Record.load(uri)" },
    { label: "load_all", detail: "function", description: "Load multiple records from the database — Record.load_all({ uri1, uri2 })" },
    { label: "save", detail: "method", description: "Save this record (creates or updates) — r:save()" },
    { label: "delete", detail: "method", description: "Delete this record from PDS and database — r:delete()" },
    { label: "set_key_type", detail: "method", description: "Set the record key type (tid, any, nsid, literal:*) — r:set_key_type(type)" },
    { label: "set_rkey", detail: "method", description: "Set a specific rkey for this record — r:set_rkey(key)" },
    { label: "set_repo", detail: "method", description: "Override the repo DID (instead of caller_did) — r:set_repo(did)" },
    { label: "generate_rkey", detail: "method", description: "Generate an rkey based on _key_type — r:generate_rkey()" },
    { label: "_uri", detail: "string?", description: "AT URI of the record (set after save)" },
    { label: "_cid", detail: "string?", description: "CID of the record (set after save)" },
    { label: "_key_type", detail: "string?", description: "Record key type from lexicon (tid, any, nsid, literal:*)" },
    { label: "_rkey", detail: "string?", description: "Record key (set via set_rkey or generate_rkey)" },
  ],
  db: [
    {
      label: "query",
      detail: "function",
      description: "Query records — db.query({ collection, did?, limit?, offset? }) → { records, cursor? }",
      insertText: "query({\n\tcollection = ${1:collection},\n})",
    },
    {
      label: "get",
      detail: "function",
      description: "Get a single record by AT URI — db.get(uri) → record or nil",
      insertText: "get(${1:uri})",
    },
    {
      label: "count",
      detail: "function",
      description: "Count records — db.count(collection, did?) → integer",
      insertText: "count(${1:collection})",
    },
    {
      label: "search",
      detail: "function",
      description: "Search records by field value — db.search({ collection, field, query, limit? }) → { records }",
      insertText: "search({\n\tcollection = ${1:collection},\n\tfield = ${2:\"field\"},\n\tquery = ${3:\"search term\"},\n})",
    },
    {
      label: "backlinks",
      detail: "function",
      description: "Find records referencing a URI — db.backlinks({ collection, uri, did?, limit?, cursor? }) → { records, cursor? }",
      insertText: "backlinks({\n\tcollection = ${1:collection},\n\turi = ${2:uri},\n})",
    },
    {
      label: "raw",
      detail: "function",
      description: "Execute a raw SQL query — db.raw(sql, params?) → rows[]",
      insertText: "raw(${1:\"SELECT * FROM records WHERE collection = ?\"}${2:, \\{${3}\\}})",
    },
    {
      label: "backend",
      detail: "function",
      description: "Returns the database backend — db.backend() → \"sqlite\" or \"postgres\"",
      insertText: "backend()",
    },
  ],
  "db.query": [
    { label: "collection", detail: "string", description: "Collection NSID (required)" },
    { label: "did", detail: "string?", description: "Filter records by DID" },
    { label: "limit", detail: "integer?", description: "Max records to return (max 100, default 20)" },
    { label: "offset", detail: "integer?", description: "Pagination offset (default 0, used with custom sort)" },
    { label: "cursor", detail: "string?", description: "Pagination cursor from previous query" },
    { label: "sort", detail: "string?", description: "Field name to sort by" },
    { label: "sortDirection", detail: "string?", description: "Sort direction — \"asc\" or \"desc\" (default \"desc\")" },
  ],
  "db.search": [
    { label: "collection", detail: "string", description: "Collection NSID (required)" },
    { label: "field", detail: "string", description: "JSON field to search (required)" },
    { label: "query", detail: "string", description: "Search term (required)" },
    { label: "limit", detail: "integer?", description: "Max records to return (max 100, default 10)" },
  ],
  "db.backlinks": [
    { label: "collection", detail: "string", description: "Collection NSID (required)" },
    { label: "uri", detail: "string", description: "Target URI to find references to (required)" },
    { label: "did", detail: "string?", description: "Filter by DID" },
    { label: "limit", detail: "integer?", description: "Max records to return (max 100, default 20)" },
    { label: "cursor", detail: "string?", description: "Pagination cursor" },
  ],
  "db.query_result": [
    { label: "records", detail: "table[]", description: "Array of record tables (each includes uri)" },
    { label: "cursor", detail: "string?", description: "Pagination cursor (present when more results exist)" },
  ],
  // Lua standard library modules
  string: [
    { label: "byte", detail: "function", description: "Returns internal numeric codes of characters — string.byte(s [, i [, j]])" },
    { label: "char", detail: "function", description: "Returns a string from character codes — string.char(···)" },
    { label: "find", detail: "function", description: "Find first match of pattern — string.find(s, pattern [, init [, plain]])" },
    { label: "format", detail: "function", description: "Format a string — string.format(formatstring, ···)" },
    { label: "gmatch", detail: "function", description: "Returns an iterator for all matches — string.gmatch(s, pattern)" },
    { label: "gsub", detail: "function", description: "Global substitution — string.gsub(s, pattern, repl [, n])" },
    { label: "len", detail: "function", description: "Returns the length of a string — string.len(s)" },
    { label: "lower", detail: "function", description: "Returns lowercase copy — string.lower(s)" },
    { label: "match", detail: "function", description: "Find first match and return captures — string.match(s, pattern [, init])" },
    { label: "rep", detail: "function", description: "Returns a repeated copy — string.rep(s, n [, sep])" },
    { label: "reverse", detail: "function", description: "Returns reversed string — string.reverse(s)" },
    { label: "sub", detail: "function", description: "Returns a substring — string.sub(s, i [, j])" },
    { label: "upper", detail: "function", description: "Returns uppercase copy — string.upper(s)" },
  ],
  table: [
    { label: "concat", detail: "function", description: "Concatenate table elements — table.concat(list [, sep [, i [, j]]])" },
    { label: "insert", detail: "function", description: "Insert element — table.insert(list, [pos,] value)" },
    { label: "move", detail: "function", description: "Move elements between tables — table.move(a1, f, e, t [, a2])" },
    { label: "pack", detail: "function", description: "Pack arguments into table with n field — table.pack(···)" },
    { label: "remove", detail: "function", description: "Remove element — table.remove(list [, pos])" },
    { label: "sort", detail: "function", description: "Sort table in-place — table.sort(list [, comp])" },
    { label: "unpack", detail: "function", description: "Unpack table elements — table.unpack(list [, i [, j]])" },
  ],
  math: [
    { label: "abs", detail: "function", description: "Absolute value — math.abs(x)" },
    { label: "acos", detail: "function", description: "Arc cosine — math.acos(x)" },
    { label: "asin", detail: "function", description: "Arc sine — math.asin(x)" },
    { label: "atan", detail: "function", description: "Arc tangent — math.atan(y [, x])" },
    { label: "ceil", detail: "function", description: "Round up — math.ceil(x)" },
    { label: "cos", detail: "function", description: "Cosine — math.cos(x)" },
    { label: "deg", detail: "function", description: "Radians to degrees — math.deg(x)" },
    { label: "exp", detail: "function", description: "e^x — math.exp(x)" },
    { label: "floor", detail: "function", description: "Round down — math.floor(x)" },
    { label: "fmod", detail: "function", description: "Remainder — math.fmod(x, y)" },
    { label: "log", detail: "function", description: "Logarithm — math.log(x [, base])" },
    { label: "max", detail: "function", description: "Maximum value — math.max(x, ···)" },
    { label: "maxinteger", detail: "number", description: "Maximum integer value" },
    { label: "min", detail: "function", description: "Minimum value — math.min(x, ···)" },
    { label: "mininteger", detail: "number", description: "Minimum integer value" },
    { label: "modf", detail: "function", description: "Integer and fractional parts — math.modf(x)" },
    { label: "rad", detail: "function", description: "Degrees to radians — math.rad(x)" },
    { label: "random", detail: "function", description: "Generate random number — math.random([m [, n]])" },
    { label: "randomseed", detail: "function", description: "Set random seed — math.randomseed([x [, y]])" },
    { label: "sin", detail: "function", description: "Sine — math.sin(x)" },
    { label: "sqrt", detail: "function", description: "Square root — math.sqrt(x)" },
    { label: "tan", detail: "function", description: "Tangent — math.tan(x)" },
    { label: "tointeger", detail: "function", description: "Convert to integer or nil — math.tointeger(x)" },
    { label: "type", detail: "function", description: "Number type (\"integer\", \"float\", or false) — math.type(x)" },
    { label: "ult", detail: "function", description: "Unsigned integer comparison — math.ult(m, n)" },
    { label: "huge", detail: "number", description: "Infinity value" },
    { label: "pi", detail: "number", description: "Pi constant (3.14159...)" },
  ],
  coroutine: [
    { label: "create", detail: "function", description: "Create a coroutine — coroutine.create(f)" },
    { label: "resume", detail: "function", description: "Resume a coroutine — coroutine.resume(co [, val1, ···])" },
    { label: "yield", detail: "function", description: "Suspend coroutine — coroutine.yield(···)" },
    { label: "status", detail: "function", description: "Coroutine status — coroutine.status(co)" },
    { label: "wrap", detail: "function", description: "Create iterator from coroutine — coroutine.wrap(f)" },
    { label: "isyieldable", detail: "function", description: "Check if running coroutine can yield — coroutine.isyieldable()" },
    { label: "running", detail: "function", description: "Returns running coroutine — coroutine.running()" },
    { label: "close", detail: "function", description: "Close a coroutine — coroutine.close(co)" },
  ],
  utf8: [
    { label: "char", detail: "function", description: "UTF-8 string from codepoints — utf8.char(···)" },
    { label: "charpattern", detail: "string", description: "Pattern matching one UTF-8 character" },
    { label: "codepoint", detail: "function", description: "Codepoints from string — utf8.codepoint(s [, i [, j [, lax]]])" },
    { label: "codes", detail: "function", description: "Iterator over UTF-8 codepoints — utf8.codes(s [, lax])" },
    { label: "len", detail: "function", description: "UTF-8 string length — utf8.len(s [, i [, j [, lax]]])" },
    { label: "offset", detail: "function", description: "Byte offset of nth character — utf8.offset(s, n [, i])" },
  ],
  // HappyView HTTP API
  http: [
    { label: "get", detail: "function", description: "HTTP GET request — http.get(url, options?) → {status, body, headers}", insertText: "get(${1:url})" },
    { label: "post", detail: "function", description: "HTTP POST request — http.post(url, options?) → {status, body, headers}", insertText: "post(${1:url}, {\n\theaders = { [\"content-type\"] = \"application/json\" },\n\tbody = ${2:body},\n})" },
    { label: "put", detail: "function", description: "HTTP PUT request — http.put(url, options?) → {status, body, headers}", insertText: "put(${1:url}, {\n\tbody = ${2:body},\n})" },
    { label: "patch", detail: "function", description: "HTTP PATCH request — http.patch(url, options?) → {status, body, headers}", insertText: "patch(${1:url}, {\n\tbody = ${2:body},\n})" },
    { label: "delete", detail: "function", description: "HTTP DELETE request — http.delete(url, options?) → {status, body, headers}", insertText: "delete(${1:url})" },
    { label: "head", detail: "function", description: "HTTP HEAD request — http.head(url, options?) → {status, headers}", insertText: "head(${1:url})" },
  ],
  "http.options": [
    { label: "headers", detail: "table?", description: "Request headers as key-value pairs" },
    { label: "body", detail: "string?", description: "Request body (for POST, PUT, PATCH, DELETE)" },
  ],
  // HappyView XRPC API
  xrpc: [
    { label: "query", detail: "function", description: "XRPC query — xrpc.query(method, params?) → {status, body}", insertText: "query(${1:\"com.atproto.repo.describeRepo\"}, {\n\t${2}\n})" },
    { label: "procedure", detail: "function", description: "XRPC procedure (requires caller_did) — xrpc.procedure(method, input, params?) → {status, body}", insertText: "procedure(${1:\"method\"}, {\n\t${2}\n})" },
  ],
  // HappyView AT Protocol API
  atproto: [
    { label: "resolve_service_endpoint", detail: "function", description: "Resolve a DID to its PDS endpoint URL — atproto.resolve_service_endpoint(did) → string or nil", insertText: "resolve_service_endpoint(${1:did})" },
    { label: "get_labels", detail: "function", description: "Get labels for a URI — atproto.get_labels(uri) → label[]", insertText: "get_labels(${1:uri})" },
    { label: "get_labels_batch", detail: "function", description: "Get labels for multiple URIs — atproto.get_labels_batch({uri1, uri2}) → {[uri]: label[]}", insertText: "get_labels_batch(${1:uris})" },
    { label: "sign", detail: "function", description: "Sign a record (requires attestation signer) — atproto.sign(record) → signature", insertText: "sign(${1:record})" },
    { label: "verify_signature", detail: "function", description: "Verify a record signature — atproto.verify_signature(record, sig, repo_did) → boolean", insertText: "verify_signature(${1:record}, ${2:sig}, ${3:repo_did})" },
    { label: "spaces", detail: "table", description: "Permissioned Spaces sub-table — atproto.spaces.*" },
  ],
  // HappyView Spaces API (atproto.spaces.*)
  spaces: [
    { label: "is_member", detail: "function", description: "Whether did is a member of the space — atproto.spaces.is_member(space_uri, did) → boolean", insertText: "is_member(${1:space_uri}, ${2:did})" },
    { label: "get_access", detail: "function", description: "Member's access level — atproto.spaces.get_access(space_uri, did) → 'read' | 'write' | nil", insertText: "get_access(${1:space_uri}, ${2:did})" },
    { label: "list_members", detail: "function", description: "List resolved members — atproto.spaces.list_members(space_uri) → {did, access}[]", insertText: "list_members(${1:space_uri})" },
    { label: "query", detail: "function", description: "List records in a space — atproto.spaces.query({space_uri, collection?, limit?, cursor?}) → {records, cursor}", insertText: "query({\n\tspace_uri = ${1:space_uri},\n\t${2}\n})" },
    { label: "get", detail: "function", description: "Look up a space (requires caller_did to write) — atproto.spaces.get(space_uri) → Space or nil", insertText: "get(${1:space_uri})" },
    { label: "create", detail: "function", description: "Create a space (requires caller_did) — atproto.spaces.create({type, skey, ...}) → Space", insertText: "create({\n\ttype = ${1:\"com.example.type\"},\n\tskey = ${2:\"skey\"},\n\t${3}\n})" },
    { label: "accept_invite", detail: "function", description: "Redeem an invite token and join a space (requires caller_did) — atproto.spaces.accept_invite({token}) → Space", insertText: "accept_invite({ token = ${1:token} })" },
  ],
  // Space handle methods (returned by atproto.spaces.get/create/accept_invite)
  Space: [
    { label: "write_record", detail: "method", description: "Create a record with an auto-generated rkey — space:write_record({collection, record}) → {uri, cid}", insertText: "write_record({\n\tcollection = ${1:\"com.example.collection\"},\n\trecord = {\n\t\t${2}\n\t},\n})" },
    { label: "put_record", detail: "method", description: "Create or overwrite a record at a specific rkey — space:put_record({collection, rkey, record, swap_cid?}) → {uri, cid}", insertText: "put_record({\n\tcollection = ${1:\"com.example.collection\"},\n\trkey = ${2:rkey},\n\trecord = {\n\t\t${3}\n\t},\n})" },
    { label: "delete_record", detail: "method", description: "Delete a record (author-ownership-only) — space:delete_record({collection, rkey, swap_cid?}) → true", insertText: "delete_record({ collection = ${1:\"com.example.collection\"}, rkey = ${2:rkey} })" },
    { label: "add_member", detail: "method", description: "Add a member (space-admin only) — space:add_member({did, access?, is_delegation?}) → {did, access}", insertText: "add_member({ did = ${1:did}, access = ${2:\"write\"} })" },
    { label: "remove_member", detail: "method", description: "Remove a member (space-admin only) — space:remove_member({did}) → true", insertText: "remove_member({ did = ${1:did} })" },
    { label: "members", detail: "method", description: "List resolved members — space:members() → {did, access}[]", insertText: "members()" },
    { label: "is_member", detail: "method", description: "Whether did is a member — space:is_member(did) → boolean", insertText: "is_member(${1:did})" },
    { label: "access", detail: "method", description: "Member's access level — space:access(did) → 'read' | 'write' | nil", insertText: "access(${1:did})" },
    { label: "update", detail: "method", description: "Update space metadata (space-admin only) — space:update({display_name?, description?, ...}) → true", insertText: "update({\n\t${1}\n})" },
    { label: "delete", detail: "method", description: "Delete the space (space-admin only) — space:delete() → true", insertText: "delete()" },
    { label: "query", detail: "method", description: "List records in the space — space:query({collection?, limit?, cursor?}) → {records, cursor}", insertText: "query({\n\t${1}\n})" },
    { label: "create_invite", detail: "method", description: "Create an invite token (space-admin only) — space:create_invite({access?, max_uses?, expires_at?}) → {invite_id, token, access, max_uses, expires_at}", insertText: "create_invite({\n\t${1}\n})" },
  ],
};

/** Extract property completions from a record schema object (`defs.main.record`). */
export function extractSchemaProperties(
  schema: Record<string, unknown> | null | undefined,
): LuaCompletionEntry[] {
  if (!schema) return [];
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const props = (schema as any)?.properties;
  if (!props || typeof props !== "object") return [];

  return Object.keys(props).map((key) => ({
    label: key,
    detail: props[key]?.type ?? "property",
    description: props[key]?.description,
  }));
}

/** Build a collection → property completions map from lexicon details.
 *  Extracts record properties from `lexicon_json.defs.main.record`. */
export function buildCollectionSchemas(
  lexicons: {
    id: string;
    lexicon_json?: Record<string, unknown> | null;
  }[],
): CollectionSchemas {
  const schemas: CollectionSchemas = {};
  for (const lex of lexicons) {
    if (!lex.lexicon_json) continue;
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const mainDef = (lex.lexicon_json as any)?.defs?.main;
    if (mainDef?.type === "record") {
      const props = extractSchemaProperties(mainDef.record);
      if (props.length) schemas[lex.id] = props;
    }
  }
  return schemas;
}

export function extractLuaCompletions(lexiconJson: string): LuaCompletions {
  const completions: LuaCompletions = { ...STATIC_COMPLETIONS };

  try {
    const parsed = JSON.parse(lexiconJson);
    const mainDef = parsed?.defs?.main;
    if (!mainDef) return completions;

    if (mainDef.type === "procedure") {
      const props = mainDef.input?.schema?.properties;
      if (props && typeof props === "object") {
        completions.input = Object.keys(props).map((key) => ({
          label: key,
          detail: props[key]?.type ?? "property",
          description: props[key]?.description,
        }));
      }
    } else if (mainDef.type === "query") {
      const props = mainDef.parameters?.properties;
      if (props && typeof props === "object") {
        completions.params = Object.keys(props).map((key) => ({
          label: key,
          detail: props[key]?.type ?? "property",
          description: props[key]?.description,
        }));
      }
    }
  } catch {
    // Invalid JSON — return static completions only
  }

  return completions;
}
