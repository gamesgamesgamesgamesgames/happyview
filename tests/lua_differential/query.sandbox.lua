function handle(input, ctx)
  local ok_db, err_db = pcall(function() return db.get("x") end)
  local ok_params, err_params = pcall(function() return params.q end)
  helper_value = 3
  log = function(m) return "mine:" .. m end
  return {
    io = type(io), debug = type(debug), package = type(package), load = type(load), loadfile = type(loadfile), dofile = type(dofile),
    collectgarbage = type(collectgarbage), os_execute = type(os.execute), os_getenv = type(os.getenv), os_exit = type(os.exit), os_remove = type(os.remove),
    unknown = type(no_such_global), numeric = type(_G[42]),
    err_db = (string.match(tostring(err_db), "the 'db'[^\n]+")), err_params = (string.match(tostring(err_params), "the 'params'[^\n]+")), shadowed = log("x"), assigned = helper_value,
    string_ok = type(string.format), table_ok = type(table.sort), math_ok = type(math.floor), print_ok = type(print), coroutine_ok = type(coroutine),
  }
end
