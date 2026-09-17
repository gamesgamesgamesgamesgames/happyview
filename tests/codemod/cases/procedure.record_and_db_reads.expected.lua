local tids = require("internal.tids")
local db = require("happyview.db")
local record = require("happyview.record")

-- codemod polyfill: v2 row shape over happyview.db; replace with the library API when convenient
local __codemod_flat, __codemod_flat_page, __codemod_flat_rows = (function()
  -- v2 handed a row back as the record body with `uri` written onto it; the
  -- library hands back an envelope around the body, and every read this
  -- script makes passes through here so it sees the shape it was written
  -- against.
  local function __codemod_flat(row)
    if row == nil then
      return nil
    end
    local body = row.record
    body.uri = row.uri
    return body
  end

  -- Rows are replaced inside the array they arrived in, so an empty list
  -- keeps the marking that encodes it as `[]`.
  local function __codemod_flat_rows(rows)
    for index = 1, #rows do
      rows[index] = __codemod_flat(rows[index])
    end
    return rows
  end

  -- The page goes back as it came, cursor and all.
  local function __codemod_flat_page(page)
    __codemod_flat_rows(page.records)
    return page
  end

  return __codemod_flat, __codemod_flat_page, __codemod_flat_rows
end)()

-- codemod polyfill: v2 Record over happyview.record; replace with the library API when convenient
local Record = (function()
  local __codemod_internal = {
    _collection = true,
    _uri = true,
    _cid = true,
    _schema = true,
    _key_type = true,
    _rkey = true,
    _repo_override = true,
  }

  local __codemod_methods = {}
  local __codemod_meta = {}

  -- The lexicon lookup is a v3 export the shim cannot do without; naming it
  -- here turns a library that predates it into a failure that says so.
  local __codemod_lexicon = record.lexicon

  -- `at://<did>/<collection>/<rkey>`: a record that has only its `_uri` still
  -- has to say which repo and key a local save goes to.
  local function __codemod_parse_uri(uri)
    return string.match(uri, "^at://([^/]+)/([^/]+)/(.+)$")
  end

  -- The record body a write sends. A `_`-prefixed key is the shim's own, and
  -- a lexicon that declares properties bounds the body to them, so a stray
  -- key on the object (a whole procedure input, say) never reaches the PDS as
  -- an undeclared field.
  local function __codemod_fields(self)
    local schema = rawget(self, "_schema")
    local allowed = nil
    if type(schema) == "table" and type(schema.properties) == "table" then
      allowed = schema.properties
    end
    local body = {}
    for key, value in pairs(self) do
      local name = key
      if type(key) == "number" then
        name = tostring(key)
      elseif type(key) ~= "string" then
        error("record field keys must be strings, got " .. type(key))
      end
      if string.sub(name, 1, 1) ~= "_" and (allowed == nil or allowed[name] ~= nil) then
        body[name] = value
      end
    end
    body["$type"] = rawget(self, "_collection")
    return body
  end

  local function __codemod_require_lexicon()
    if __codemod_lexicon == nil then
      error("the Record polyfill needs the happyview-record that exports lexicon()")
    end
  end

  local function __codemod_main_def(collection)
    __codemod_require_lexicon()
    local lexicon = __codemod_lexicon(collection)
    if type(lexicon) ~= "table" or type(lexicon.defs) ~= "table" then
      return nil
    end
    local main = lexicon.defs.main
    if type(main) == "table" then
      return main
    end
    return nil
  end

  local function __codemod_build(collection, uri, main)
    local self = setmetatable({}, __codemod_meta)
    rawset(self, "_collection", collection)
    rawset(self, "_uri", uri)
    if main ~= nil then
      rawset(self, "_schema", main.record)
    end
    return self
  end

  local function __codemod_literal(key_type)
    if type(key_type) ~= "string" then
      return nil
    end
    return string.match(key_type, "^literal:(.+)$")
  end

  local function __codemod_mint_rkey(key_type, nsid_message)
    if key_type == "tid" or key_type == "any" then
      return tids.create()
    end
    local literal = __codemod_literal(key_type)
    if literal ~= nil then
      return literal
    end
    if key_type == "nsid" then
      error(nsid_message)
    end
    error("unknown key type '" .. tostring(key_type) .. "'")
  end

  function __codemod_meta.__index(self, key)
    return __codemod_methods[key]
  end

  function __codemod_meta.__newindex(self, key, value)
    if __codemod_internal[key] then
      error("cannot assign to internal field '" .. key .. "'")
    end
    rawset(self, key, value)
  end

  function __codemod_meta.__tostring(self)
    local uri = rawget(self, "_uri")
    if uri == nil then
      return "Record(" .. rawget(self, "_collection") .. ") [unsaved]"
    end
    return "Record(" .. rawget(self, "_collection") .. ") [uri=" .. uri .. "]"
  end

  function __codemod_methods.set_key_type(self, key_type)
    if key_type ~= "tid" and key_type ~= "any" and key_type ~= "nsid" and __codemod_literal(key_type) == nil then
      error("invalid key type '" .. tostring(key_type) .. "': expected tid, any, nsid, or literal:*")
    end
    rawset(self, "_key_type", key_type)
    return self
  end

  function __codemod_methods.set_rkey(self, key)
    if type(key) == "number" then
      key = tostring(key)
    end
    if type(key) ~= "string" or key == "" then
      error("rkey must be a non-empty string")
    end
    rawset(self, "_rkey", key)
    return self
  end

  -- The repo is only checked where the write happens: the host raises the
  -- same refusal, naming the repo and pointing at linked_repos.
  function __codemod_methods.set_repo(self, did)
    if type(did) ~= "string" or did == "" then
      error("did must be a non-empty string")
    end
    rawset(self, "_repo_override", did)
    return self
  end

  function __codemod_methods.generate_rkey(self)
    local key_type = rawget(self, "_key_type")
    if key_type == nil then
      error("no _key_type set — call set_key_type() first or use a record-type lexicon")
    end
    local rkey = __codemod_mint_rkey(key_type, "cannot auto-generate rkey for nsid key type — use set_rkey() instead")
    rawset(self, "_rkey", rkey)
    return rkey
  end

  local function __codemod_save(self)
    local uri = rawget(self, "_uri")
    local ref
    if uri == nil then
      ref = record.create(rawget(self, "_collection"), __codemod_fields(self), {
        rkey = rawget(self, "_rkey"),
        repo = rawget(self, "_repo_override"),
      })
    else
      ref = record.put(uri, __codemod_fields(self))
    end
    rawset(self, "_uri", ref.uri)
    rawset(self, "_cid", ref.cid)
    return ref
  end

  function __codemod_methods.save(self)
    __codemod_save(self)
    return self
  end

  -- v2 refused to delete anything without a caller, and dropped the index
  -- row whatever else the PDS answered, because "remove it from view" stays
  -- meaningful either way.
  function __codemod_methods.delete(self)
    local uri = rawget(self, "_uri")
    if uri == nil then
      error("cannot delete a Record that has no _uri")
    end
    local ok, err = pcall(record.delete, uri)
    if not ok and string.find(tostring(err), "NO_SESSION", 1, true) then
      error("no PDS auth in this script context — use :save_local() / :delete_local() / Record.delete_local(uri) for local-only mutation")
    end
    record.delete_local(uri)
    rawset(self, "_uri", nil)
    rawset(self, "_cid", nil)
    return self
  end

  function __codemod_methods.save_local(self)
    local uri = rawget(self, "_uri")
    local did, rkey
    if uri == nil then
      did = rawget(self, "_repo_override")
      rkey = rawget(self, "_rkey")
      if rkey == nil then
        rkey = __codemod_mint_rkey(rawget(self, "_key_type") or "tid", "cannot auto-generate rkey for nsid key type — call set_rkey() first")
      end
    else
      local parsed_did, _, parsed_rkey = __codemod_parse_uri(uri)
      if parsed_did == nil then
        error("invalid AT URI: " .. uri)
      end
      did = parsed_did
      rkey = parsed_rkey
    end
    local ref = record.save_local(rawget(self, "_collection"), rkey, __codemod_fields(self), did)
    rawset(self, "_uri", ref.uri)
    return self
  end

  function __codemod_methods.delete_local(self)
    local uri = rawget(self, "_uri")
    if uri == nil then
      error("cannot delete_local a Record that has no _uri")
    end
    record.delete_local(uri)
    rawset(self, "_uri", nil)
    rawset(self, "_cid", nil)
    return self
  end

  local __codemod_Record = {}

  function __codemod_Record.load(uri)
    __codemod_require_lexicon()
    local row = record.load(uri)
    if row == nil then
      return nil
    end
    local self = __codemod_build(row.collection, row.uri, __codemod_main_def(row.collection))
    -- v2 read the cid column as a string, so a row written only by
    -- `save_local` carried an empty one; an envelope with no cid reads the
    -- same here.
    rawset(self, "_cid", row.cid or "")
    for key, value in pairs(row.record) do
      -- `$type` comes back on save from the collection.
      if key ~= "$type" then
        rawset(self, key, value)
      end
    end
    return self
  end

  -- A URI with nothing behind it leaves its own slot empty rather than
  -- shortening the list, so results line up with the URIs asked for.
  function __codemod_Record.load_all(uris)
    local loaded = {}
    for index = 1, #uris do
      loaded[index] = __codemod_Record.load(uris[index])
    end
    return loaded
  end

  function __codemod_Record.save_all(records)
    local refs = {}
    for index = 1, #records do
      refs[index] = __codemod_save(records[index])
    end
    return refs
  end

  function __codemod_Record.delete_local(uri)
    return record.delete_local(uri)
  end

  function __codemod_Record.new(collection, fields)
    return __codemod_Record(collection, fields)
  end

  return setmetatable(__codemod_Record, {
    __call = function(_, collection, fields)
      if type(collection) ~= "string" or collection == "" then
        error("Record(collection, fields): collection must be a non-empty string")
      end
      local main = __codemod_main_def(collection)
      local self = __codemod_build(collection, nil, main)
      if main ~= nil then
        rawset(self, "_key_type", main.key)
      end
      if type(fields) == "table" then
        for key, value in pairs(fields) do
          rawset(self, key, value)
        end
      end
      local schema = rawget(self, "_schema")
      if type(schema) == "table" and type(schema.properties) == "table" then
        for key, property in pairs(schema.properties) do
          if type(property) ~= "table" then
            error("lexicon property '" .. tostring(key) .. "' is not an object")
          end
          if string.sub(key, 1, 1) ~= "_" and rawget(self, key) == nil and property.default ~= nil then
            rawset(self, key, property.default)
          end
        end
      end
      return self
    end,
  })
end)()

function handle(input, ctx)
  local r = Record.load(input.uri)
  local row = __codemod_flat(db.get(input.uri))
  return { cid = r._cid, title = row.title }
end
