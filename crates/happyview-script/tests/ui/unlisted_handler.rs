use happyview_script::{export, query, Ctx, Result, Value};

#[query("app.test.listed")]
async fn listed(input: Value, _ctx: Ctx) -> Result<Value> { Ok(input) }

#[query("app.test.forgotten")]
async fn forgotten(input: Value, _ctx: Ctx) -> Result<Value> { Ok(input) }

export!(listed);

fn main() {}
