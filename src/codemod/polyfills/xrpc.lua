local xrpc = (function()
  -- v2 answered an HTTP outcome as `{status, body}` and raised for everything
  -- else; v3 raises for all of them, carrying the far side's status inside the
  -- message. Every raise comes back as a status here, 500 when the message
  -- names none, so a script's own status guard still decides.
  local function __codemod_status(message)
    local code = string.match(message, "XRPC_ERROR[^%d]*(%d+)")
      or string.match(message, "PDS_ERROR[^%d]*(%d+)")
    return tonumber(code) or 500
  end

  local function __codemod_call(method, nsid, ...)
    local ok, result = pcall(method, nsid, ...)
    if not ok then
      local message = tostring(result)
      return { status = __codemod_status(message), body = message }
    end
    -- v2's body was the response bytes, so a caller decoding it has to find a
    -- string here rather than the table the library returns.
    return { status = 200, body = json.encode(result) }
  end

  return {
    query = function(nsid, params)
      return __codemod_call(xrpc.query, nsid, params)
    end,
    procedure = function(nsid, input, params)
      return __codemod_call(xrpc.procedure, nsid, input, params)
    end,
  }
end)()
