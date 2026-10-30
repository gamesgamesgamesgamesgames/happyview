function handle()
  return db.query({
    collection = "app.example.post",
    filter = { combine = "AND", { field = "a", value = 1 }, { field = "b", value = 2 } },
  })
end
