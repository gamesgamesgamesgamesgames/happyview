function handle()
  return db.query({
    collection = "app.example.post",
    limit = TID.toNumber(params.n),
  })
end
