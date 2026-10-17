# happyview-provider

The contract every HappyView record source and record store implements. It defines the traits (`RecordSource`, `RecordStorage`, `Host`), the values that cross the boundary between HappyView and a provider, and the database helpers a provider needs to run on both SQLite and Postgres.

This crate is dual-licensed under MIT OR Apache-2.0, independently of HappyView's own licence, so providers can be built and distributed under any terms.
