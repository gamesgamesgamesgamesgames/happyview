use happyview_script::{export, Ctx, Result, Value};

async fn plain(input: Value, _ctx: Ctx) -> Result<Value> { Ok(input) }

export!(plain);

fn main() {}
