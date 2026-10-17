# happyview-source-jetstream

The Jetstream record source for HappyView: the Jetstream v2 archive client,
segment reader and the pure parts of the live consumer (settings, wire events,
the applied watermark, zstd frames).

It depends on `happyview-provider` and external crates only, never on
`happyview`.

## Licence

This crate follows the repository's licence. It is marked `MIT` for now and
will be changed to match the repository when it is relicensed.
