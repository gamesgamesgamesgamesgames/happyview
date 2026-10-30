---
title: "Standard Libraries"
---

The following Lua 5.4 standard library modules and builtins are available in the HappyView sandbox.

## string

- [`byte`](https://lua.org/manual/5.4/manual.html#pdf-string.byte)
- [`char`](https://lua.org/manual/5.4/manual.html#pdf-string.char)
- [`find`](https://lua.org/manual/5.4/manual.html#pdf-string.find)
- [`format`](https://lua.org/manual/5.4/manual.html#pdf-string.format)
- [`gmatch`](https://lua.org/manual/5.4/manual.html#pdf-string.gmatch)
- [`gsub`](https://lua.org/manual/5.4/manual.html#pdf-string.gsub)
- [`len`](https://lua.org/manual/5.4/manual.html#pdf-string.len)
- [`lower`](https://lua.org/manual/5.4/manual.html#pdf-string.lower)
- [`match`](https://lua.org/manual/5.4/manual.html#pdf-string.match)
- [`rep`](https://lua.org/manual/5.4/manual.html#pdf-string.rep)
- [`reverse`](https://lua.org/manual/5.4/manual.html#pdf-string.reverse)
- [`sub`](https://lua.org/manual/5.4/manual.html#pdf-string.sub)
- [`upper`](https://lua.org/manual/5.4/manual.html#pdf-string.upper)

## table

- [`concat`](https://lua.org/manual/5.4/manual.html#pdf-table.concat)
- [`insert`](https://lua.org/manual/5.4/manual.html#pdf-table.insert)
- [`remove`](https://lua.org/manual/5.4/manual.html#pdf-table.remove)
- [`sort`](https://lua.org/manual/5.4/manual.html#pdf-table.sort)
- [`unpack`](https://lua.org/manual/5.4/manual.html#pdf-table.unpack)

## math

- [`abs`](https://lua.org/manual/5.4/manual.html#pdf-math.abs)
- [`ceil`](https://lua.org/manual/5.4/manual.html#pdf-math.ceil)
- [`floor`](https://lua.org/manual/5.4/manual.html#pdf-math.floor)
- [`max`](https://lua.org/manual/5.4/manual.html#pdf-math.max)
- [`min`](https://lua.org/manual/5.4/manual.html#pdf-math.min)
- [`random`](https://lua.org/manual/5.4/manual.html#pdf-math.random)
- [`sqrt`](https://lua.org/manual/5.4/manual.html#pdf-math.sqrt)
- [`huge`](https://lua.org/manual/5.4/manual.html#pdf-math.huge)
- [`pi`](https://lua.org/manual/5.4/manual.html#pdf-math.pi)

## os (safe subset)

Only the following safe functions are available from the `os` module:

- [`time`](https://lua.org/manual/5.4/manual.html#pdf-os.time)
- [`date`](https://lua.org/manual/5.4/manual.html#pdf-os.date)
- [`difftime`](https://lua.org/manual/5.4/manual.html#pdf-os.difftime)
- [`clock`](https://lua.org/manual/5.4/manual.html#pdf-os.clock)

Dangerous functions like `os.execute`, `os.remove`, `os.rename`, and `os.exit` are not available.

## Builtins

- [`print`](https://lua.org/manual/5.4/manual.html#pdf-print)
- [`tostring`](https://lua.org/manual/5.4/manual.html#pdf-tostring)
- [`tonumber`](https://lua.org/manual/5.4/manual.html#pdf-tonumber)
- [`type`](https://lua.org/manual/5.4/manual.html#pdf-type)
- [`pairs`](https://lua.org/manual/5.4/manual.html#pdf-pairs)
- [`ipairs`](https://lua.org/manual/5.4/manual.html#pdf-ipairs)
- [`next`](https://lua.org/manual/5.4/manual.html#pdf-next)
- [`select`](https://lua.org/manual/5.4/manual.html#pdf-select)
- [`unpack`](https://lua.org/manual/5.4/manual.html#pdf-table.unpack)
- [`error`](https://lua.org/manual/5.4/manual.html#pdf-error)
- [`pcall`](https://lua.org/manual/5.4/manual.html#pdf-pcall)
- [`xpcall`](https://lua.org/manual/5.4/manual.html#pdf-xpcall)
- [`assert`](https://lua.org/manual/5.4/manual.html#pdf-assert)
- [`setmetatable`](https://lua.org/manual/5.4/manual.html#pdf-setmetatable)
- [`getmetatable`](https://lua.org/manual/5.4/manual.html#pdf-getmetatable)
- [`rawget`](https://lua.org/manual/5.4/manual.html#pdf-rawget)
- [`rawset`](https://lua.org/manual/5.4/manual.html#pdf-rawset)
- [`rawequal`](https://lua.org/manual/5.4/manual.html#pdf-rawequal)

## Removed modules

The following standard Lua modules are **removed** and unavailable in the sandbox:

`io`, `debug`, `package`, `dofile`, `loadfile`, `load`, `collectgarbage`

Lua's own `require` is removed too, but HappyView installs its own in its place: it loads installed library plugins and the `internal.*` built-ins — see [Built-in Modules](built-in-modules.md).
