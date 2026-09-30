-- What counts as an array going out: sequences, mixed tables, holes, integer
-- keys that are not a sequence, and the array metatable surviving edits.
local json = require("internal.json")
function handle(input, ctx)
  return {
    seq = { 1, 2, 3 },
    mixed = { 1, 2, x = "dropped" },
    hole = { 1, nil, 3 },
    sparse = { [1] = "a", [3] = "c" },
    int_keys_only = { [5] = "five", [7] = "seven" },
    string_int = { ["1"] = "a" },
    bool_value = { ok = true, no = false },
    nested = { rows = { { id = 1 }, { id = 2 } } },
    to_array_holes = json.to_array({ 1, 2, nil, 4 }),
    to_array_mixed = json.to_array({ "a", "b", k = "v" }),
    input_items_type = type(input.items),
    input_items_len = #input.items,
    has_array_mt = getmetatable(input.items) ~= nil,
    mt_locked = getmetatable(input.items),
    plain_has_mt = getmetatable({}) ~= nil,
  }
end
