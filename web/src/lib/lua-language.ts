// Lua's own keywords, builtins and snippets, which every script editor
// offers. What HappyView's libraries offer comes from the declarations the
// interpreter renders, read by `lib/luals`.

export interface LuaSnippetEntry {
  label: string;
  insertText: string;
  detail: string;
  description?: string;
}

export const LUA_KEYWORDS = [
  "and",
  "break",
  "do",
  "else",
  "elseif",
  "end",
  "false",
  "for",
  "function",
  "goto",
  "if",
  "in",
  "local",
  "nil",
  "not",
  "or",
  "repeat",
  "return",
  "then",
  "true",
  "until",
  "while",
];

export const LUA_BUILTINS = [
  "print",
  "tostring",
  "tonumber",
  "type",
  "pairs",
  "ipairs",
  "next",
  "select",
  "unpack",
  "error",
  "pcall",
  "xpcall",
  "assert",
  "setmetatable",
  "getmetatable",
  "rawget",
  "rawset",
  "rawequal",
  "require",
  // Standard library modules
  "string",
  "table",
  "math",
  "coroutine",
  "utf8",
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
    insertText:
      "if ${1:condition} then\n\t$2\nelseif ${3:condition} then\n\t$0\nend",
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
