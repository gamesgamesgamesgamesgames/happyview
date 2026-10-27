export interface HoverEntry {
  signature: string;
  description: string;
  module?: string;
}

export const HOVER_DOCS = new Map<string, HoverEntry>([
  // ── Lua keywords ──────────────────────────────────────────────────────
  [
    "and",
    {
      signature: "and",
      description:
        "Logical AND operator — returns first falsy operand or last operand",
    },
  ],
  ["break", { signature: "break", description: "Exit the innermost loop" }],
  [
    "do",
    {
      signature: "do ... end",
      description: "Block delimiter — creates a new scope",
    },
  ],
  [
    "else",
    { signature: "else", description: "Alternative branch in an if statement" },
  ],
  [
    "elseif",
    {
      signature: "elseif condition then",
      description: "Additional condition branch in an if statement",
    },
  ],
  [
    "end",
    {
      signature: "end",
      description: "Closes a block (function, if, for, while, do)",
    },
  ],
  ["false", { signature: "false", description: "Boolean false value" }],
  [
    "for",
    {
      signature: "for var = start, stop [, step] do ... end",
      description: "Numeric or generic loop",
    },
  ],
  [
    "function",
    {
      signature: "function name(...) ... end",
      description: "Define a function",
    },
  ],
  [
    "goto",
    {
      signature: "goto label",
      description: "Jump to a label within the same block",
    },
  ],
  [
    "if",
    {
      signature: "if condition then ... end",
      description: "Conditional branch",
    },
  ],
  [
    "in",
    { signature: "in", description: "Iterator separator in generic for loops" },
  ],
  [
    "local",
    {
      signature: "local name [= value]",
      description: "Declare a local variable",
    },
  ],
  [
    "nil",
    {
      signature: "nil",
      description: "The nil value — represents absence of a value",
    },
  ],
  ["not", { signature: "not", description: "Logical NOT operator" }],
  [
    "or",
    {
      signature: "or",
      description:
        "Logical OR operator — returns first truthy operand or last operand",
    },
  ],
  [
    "repeat",
    {
      signature: "repeat ... until condition",
      description: "Loop that runs at least once",
    },
  ],
  [
    "return",
    {
      signature: "return [values]",
      description: "Return values from a function",
    },
  ],
  [
    "then",
    {
      signature: "then",
      description: "Begins the body of an if/elseif branch",
    },
  ],
  ["true", { signature: "true", description: "Boolean true value" }],
  [
    "until",
    {
      signature: "until condition",
      description: "End condition of a repeat loop",
    },
  ],
  [
    "while",
    {
      signature: "while condition do ... end",
      description: "Loop while condition is true",
    },
  ],

  // ── Global builtins ───────────────────────────────────────────────────
  [
    "print",
    {
      signature: "print(···)",
      description: "Print values to stdout (separated by tabs)",
    },
  ],
  [
    "tostring",
    { signature: "tostring(v)", description: "Convert any value to a string" },
  ],
  [
    "tonumber",
    {
      signature: "tonumber(e [, base])",
      description: "Convert a value to a number, or nil if not convertible",
    },
  ],
  [
    "type",
    {
      signature: "type(v)",
      description: "Returns the type of a value as a string",
    },
  ],
  [
    "pairs",
    {
      signature: "pairs(t)",
      description: "Returns an iterator over all key-value pairs in a table",
    },
  ],
  [
    "ipairs",
    {
      signature: "ipairs(t)",
      description:
        "Returns an iterator over integer-keyed elements (1, 2, ...)",
    },
  ],
  [
    "next",
    {
      signature: "next(table [, index])",
      description: "Returns the next key-value pair after the given key",
    },
  ],
  [
    "select",
    {
      signature: "select(index, ···)",
      description: "Returns arguments after the given index, or count with '#'",
    },
  ],
  [
    "unpack",
    {
      signature: "unpack(list [, i [, j]])",
      description: "Returns elements from a table (alias for table.unpack)",
    },
  ],
  [
    "error",
    {
      signature: "error(message [, level])",
      description: "Raise an error with a message",
    },
  ],
  [
    "pcall",
    {
      signature: "pcall(f [, arg1, ···])",
      description: "Call a function in protected mode — returns ok, result",
    },
  ],
  [
    "xpcall",
    {
      signature: "xpcall(f, msgh [, arg1, ···])",
      description: "Like pcall but with a custom error handler",
    },
  ],
  [
    "assert",
    {
      signature: "assert(v [, message])",
      description: "Raise an error if v is falsy",
    },
  ],
  [
    "setmetatable",
    {
      signature: "setmetatable(table, metatable)",
      description: "Set the metatable for a table",
    },
  ],
  [
    "getmetatable",
    {
      signature: "getmetatable(object)",
      description: "Returns the metatable of an object, or nil",
    },
  ],
  [
    "rawget",
    {
      signature: "rawget(table, index)",
      description: "Get a value without invoking __index metamethod",
    },
  ],
  [
    "rawset",
    {
      signature: "rawset(table, index, value)",
      description: "Set a value without invoking __newindex metamethod",
    },
  ],
  [
    "rawequal",
    {
      signature: "rawequal(v1, v2)",
      description: "Check equality without invoking __eq metamethod",
    },
  ],

  // ── string module ─────────────────────────────────────────────────────
  [
    "string.byte",
    {
      signature: "string.byte(s [, i [, j]])",
      description: "Returns internal numeric codes of characters",
      module: "string",
    },
  ],
  [
    "string.char",
    {
      signature: "string.char(···)",
      description: "Returns a string from character codes",
      module: "string",
    },
  ],
  [
    "string.find",
    {
      signature: "string.find(s, pattern [, init [, plain]])",
      description: "Find first match of pattern in string",
      module: "string",
    },
  ],
  [
    "string.format",
    {
      signature: "string.format(formatstring, ···)",
      description: "Format a string (like sprintf)",
      module: "string",
    },
  ],
  [
    "string.gmatch",
    {
      signature: "string.gmatch(s, pattern)",
      description: "Returns an iterator for all pattern matches",
      module: "string",
    },
  ],
  [
    "string.gsub",
    {
      signature: "string.gsub(s, pattern, repl [, n])",
      description: "Global substitution — replace pattern matches",
      module: "string",
    },
  ],
  [
    "string.len",
    {
      signature: "string.len(s)",
      description: "Returns the byte length of a string",
      module: "string",
    },
  ],
  [
    "string.lower",
    {
      signature: "string.lower(s)",
      description: "Returns lowercase copy of a string",
      module: "string",
    },
  ],
  [
    "string.match",
    {
      signature: "string.match(s, pattern [, init])",
      description: "Find first match and return captures",
      module: "string",
    },
  ],
  [
    "string.rep",
    {
      signature: "string.rep(s, n [, sep])",
      description: "Returns a repeated copy of a string",
      module: "string",
    },
  ],
  [
    "string.reverse",
    {
      signature: "string.reverse(s)",
      description: "Returns reversed string",
      module: "string",
    },
  ],
  [
    "string.sub",
    {
      signature: "string.sub(s, i [, j])",
      description: "Returns a substring",
      module: "string",
    },
  ],
  [
    "string.upper",
    {
      signature: "string.upper(s)",
      description: "Returns uppercase copy of a string",
      module: "string",
    },
  ],

  // ── table module ──────────────────────────────────────────────────────
  [
    "table.concat",
    {
      signature: "table.concat(list [, sep [, i [, j]]])",
      description: "Concatenate table elements into a string",
      module: "table",
    },
  ],
  [
    "table.insert",
    {
      signature: "table.insert(list, [pos,] value)",
      description: "Insert an element into a table",
      module: "table",
    },
  ],
  [
    "table.move",
    {
      signature: "table.move(a1, f, e, t [, a2])",
      description: "Move elements between tables",
      module: "table",
    },
  ],
  [
    "table.pack",
    {
      signature: "table.pack(···)",
      description: "Pack arguments into a table with n field",
      module: "table",
    },
  ],
  [
    "table.remove",
    {
      signature: "table.remove(list [, pos])",
      description: "Remove an element from a table",
      module: "table",
    },
  ],
  [
    "table.sort",
    {
      signature: "table.sort(list [, comp])",
      description: "Sort a table in-place",
      module: "table",
    },
  ],
  [
    "table.unpack",
    {
      signature: "table.unpack(list [, i [, j]])",
      description: "Unpack table elements as multiple return values",
      module: "table",
    },
  ],

  // ── math module ───────────────────────────────────────────────────────
  [
    "math.abs",
    { signature: "math.abs(x)", description: "Absolute value", module: "math" },
  ],
  [
    "math.acos",
    {
      signature: "math.acos(x)",
      description: "Arc cosine (in radians)",
      module: "math",
    },
  ],
  [
    "math.asin",
    {
      signature: "math.asin(x)",
      description: "Arc sine (in radians)",
      module: "math",
    },
  ],
  [
    "math.atan",
    {
      signature: "math.atan(y [, x])",
      description: "Arc tangent of y/x (in radians)",
      module: "math",
    },
  ],
  [
    "math.ceil",
    {
      signature: "math.ceil(x)",
      description: "Round up to nearest integer",
      module: "math",
    },
  ],
  [
    "math.cos",
    {
      signature: "math.cos(x)",
      description: "Cosine (x in radians)",
      module: "math",
    },
  ],
  [
    "math.deg",
    {
      signature: "math.deg(x)",
      description: "Convert radians to degrees",
      module: "math",
    },
  ],
  [
    "math.exp",
    { signature: "math.exp(x)", description: "Returns e^x", module: "math" },
  ],
  [
    "math.floor",
    {
      signature: "math.floor(x)",
      description: "Round down to nearest integer",
      module: "math",
    },
  ],
  [
    "math.fmod",
    {
      signature: "math.fmod(x, y)",
      description: "Remainder of x / y",
      module: "math",
    },
  ],
  [
    "math.huge",
    {
      signature: "math.huge",
      description: "Infinity value (HUGE_VAL)",
      module: "math",
    },
  ],
  [
    "math.log",
    {
      signature: "math.log(x [, base])",
      description: "Logarithm of x in the given base (default: e)",
      module: "math",
    },
  ],
  [
    "math.max",
    {
      signature: "math.max(x, ···)",
      description: "Returns the maximum value among arguments",
      module: "math",
    },
  ],
  [
    "math.maxinteger",
    {
      signature: "math.maxinteger",
      description: "Maximum integer value (2^63 - 1)",
      module: "math",
    },
  ],
  [
    "math.min",
    {
      signature: "math.min(x, ···)",
      description: "Returns the minimum value among arguments",
      module: "math",
    },
  ],
  [
    "math.mininteger",
    {
      signature: "math.mininteger",
      description: "Minimum integer value (-2^63)",
      module: "math",
    },
  ],
  [
    "math.modf",
    {
      signature: "math.modf(x)",
      description: "Returns integer and fractional parts of x",
      module: "math",
    },
  ],
  [
    "math.pi",
    {
      signature: "math.pi",
      description: "Pi constant (3.14159...)",
      module: "math",
    },
  ],
  [
    "math.rad",
    {
      signature: "math.rad(x)",
      description: "Convert degrees to radians",
      module: "math",
    },
  ],
  [
    "math.random",
    {
      signature: "math.random([m [, n]])",
      description: "Generate a pseudo-random number",
      module: "math",
    },
  ],
  [
    "math.randomseed",
    {
      signature: "math.randomseed([x [, y]])",
      description: "Set the random seed",
      module: "math",
    },
  ],
  [
    "math.sin",
    {
      signature: "math.sin(x)",
      description: "Sine (x in radians)",
      module: "math",
    },
  ],
  [
    "math.sqrt",
    {
      signature: "math.sqrt(x)",
      description: "Square root of x",
      module: "math",
    },
  ],
  [
    "math.tan",
    {
      signature: "math.tan(x)",
      description: "Tangent (x in radians)",
      module: "math",
    },
  ],
  [
    "math.tointeger",
    {
      signature: "math.tointeger(x)",
      description: "Convert to integer if possible, otherwise nil",
      module: "math",
    },
  ],
  [
    "math.type",
    {
      signature: "math.type(x)",
      description: 'Returns "integer", "float", or false',
      module: "math",
    },
  ],
  [
    "math.ult",
    {
      signature: "math.ult(m, n)",
      description: "Unsigned integer less-than comparison",
      module: "math",
    },
  ],

  // ── coroutine module ──────────────────────────────────────────────────
  [
    "coroutine.create",
    {
      signature: "coroutine.create(f)",
      description: "Create a new coroutine from function f",
      module: "coroutine",
    },
  ],
  [
    "coroutine.resume",
    {
      signature: "coroutine.resume(co [, val1, ···])",
      description: "Resume a suspended coroutine",
      module: "coroutine",
    },
  ],
  [
    "coroutine.yield",
    {
      signature: "coroutine.yield(···)",
      description: "Suspend the running coroutine",
      module: "coroutine",
    },
  ],
  [
    "coroutine.status",
    {
      signature: "coroutine.status(co)",
      description:
        'Returns coroutine status: "running", "suspended", "normal", or "dead"',
      module: "coroutine",
    },
  ],
  [
    "coroutine.wrap",
    {
      signature: "coroutine.wrap(f)",
      description: "Create an iterator function from a coroutine",
      module: "coroutine",
    },
  ],
  [
    "coroutine.isyieldable",
    {
      signature: "coroutine.isyieldable()",
      description: "Returns true if the running coroutine can yield",
      module: "coroutine",
    },
  ],
  [
    "coroutine.running",
    {
      signature: "coroutine.running()",
      description: "Returns the running coroutine and a boolean (true if main)",
      module: "coroutine",
    },
  ],
  [
    "coroutine.close",
    {
      signature: "coroutine.close(co)",
      description: "Close a coroutine and release its resources",
      module: "coroutine",
    },
  ],

  // ── utf8 module ───────────────────────────────────────────────────────
  [
    "utf8.char",
    {
      signature: "utf8.char(···)",
      description: "Returns a UTF-8 string from codepoint values",
      module: "utf8",
    },
  ],
  [
    "utf8.charpattern",
    {
      signature: "utf8.charpattern",
      description: "Pattern that matches exactly one UTF-8 character",
      module: "utf8",
    },
  ],
  [
    "utf8.codepoint",
    {
      signature: "utf8.codepoint(s [, i [, j [, lax]]])",
      description: "Returns codepoint values of characters in a string",
      module: "utf8",
    },
  ],
  [
    "utf8.codes",
    {
      signature: "utf8.codes(s [, lax])",
      description: "Returns an iterator over UTF-8 codepoints",
      module: "utf8",
    },
  ],
  [
    "utf8.len",
    {
      signature: "utf8.len(s [, i [, j [, lax]]])",
      description: "Returns the number of UTF-8 characters in a string",
      module: "utf8",
    },
  ],
  [
    "utf8.offset",
    {
      signature: "utf8.offset(s, n [, i])",
      description: "Returns the byte position of the nth UTF-8 character",
      module: "utf8",
    },
  ],
]);
