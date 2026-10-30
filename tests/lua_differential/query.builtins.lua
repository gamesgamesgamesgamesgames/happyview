local time = require("internal.time")
local tids = require("internal.tids")
local json = require("internal.json")
local log = require("internal.logging")
function handle(input, ctx)
  log.debug("dbg")
  log.info("hello", { uri = input.uri, n = 2, list = { 1, 2 }, empty = {} })
  log.warn("careful")
  log.error("bad", { nested = { a = { b = 1 } } })
  local now = time.now()
  local tid = tids.to_tid(1757775845000)
  local ok_tid, bad_tid = pcall(tids.from_tid, "nope")
  local ok_enc, bad_enc = pcall(json.encode, { f = print })
  local ok_dec, bad_dec = pcall(json.decode, "not json")
  local ok_rec, bad_rec = pcall(function() local t = {}; t.self = t; return json.encode(t) end)
  local decoded = json.decode('{"a":[1,2,{"b":null}],"c":null,"d":"é😀"}')
  return {
    now = now, iso = time.to_iso8601(now), iso_epoch = time.to_iso8601(0), iso_neg = time.to_iso8601(-1),
    parsed = time.from_iso8601(input.when), parsed_offset = time.from_iso8601("2026-09-17T12:20:30+02:00"), parsed_nofrac = time.from_iso8601("2026-09-17T10:20:30Z"),
    parsed_micro = time.from_iso8601("2026-09-17T10:20:30.123456Z"), parsed_bad = time.from_iso8601("soon") == nil, parsed_date_only = time.from_iso8601("2026-09-17") == nil,
    tid = tid, tid_back = tids.from_tid(tid), created_len = #tids.create(), ok_tid = ok_tid, bad_tid = (string.match(tostring(bad_tid), "invalid TID: %w+")),
    ok_enc = ok_enc, bad_enc = (string.match(tostring(bad_enc), "json.encode: [^\n]+")), ok_dec = ok_dec, ok_rec = ok_rec,
    c_type = type(decoded.c), b_type = type(decoded.a[3].b), reencoded = json.encode(decoded.a), unicode = decoded.d, unicode_len = #decoded.d,
    os_time = os.time(), os_date = os.date("!%Y-%m-%dT%H:%M:%SZ", os.time()), os_members = (function() local n = {}; for k in pairs(os) do n[#n + 1] = k end; table.sort(n); return n end)(),
  }
end
