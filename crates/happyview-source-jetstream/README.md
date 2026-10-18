# happyview-source-jetstream

The Jetstream record source for HappyView: the Jetstream v2 archive client,
segment reader and the pure parts of the live consumer (settings, wire events,
the applied watermark, zstd frames).

It depends on `happyview-provider` and external crates only, never on
`happyview`.

See [Record providers](../../packages/docs/content/docs/guides/record-providers.md) for how to compose this source into another provider with `JetstreamSource::builder()`, `live_filter` and `executor_for`. Only the crate-root re-exports are stable API; modules marked `#[doc(hidden)]` are not.

## Licence

This crate follows the repository's licence. It is marked `MIT` for now and
will be changed to match the repository when it is relicensed.
