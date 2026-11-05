use happyview_script::{export, query, Ctx, Result, Value};

#[query("app.test.sync")]
fn sync_handler(input: Value, _ctx: Ctx) -> Result<Value> { Ok(input) }

export!();

fn main() {}
