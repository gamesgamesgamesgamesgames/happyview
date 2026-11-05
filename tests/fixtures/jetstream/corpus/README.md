# Jetstream golden corpus

Vendored from bluesky-social/jetstream at 08bef94034c35732a7048efe5790511470ddab39
(v0.4.1), `internal/corpus/testdata/`. Copyright (c) 2022-2026 Bluesky Social PBC,
and Contributors; dual-licensed MIT OR Apache-2.0.

- `golden_corpus_segment.jss`: a sealed segment written by a known-good Jetstream
  build. 23 create events from one repo (`did:plc:53iucmf5ldftgqyf2ac7hmhd`), one per
  record in MST key order, seqs 1-23, witnessed_at 1_750_000_000_000_000 + seq,
  8 events per block.
- `car_expected_records.tsv`: `collection/rkey<TAB>record CID` for those records,
  from `goat repo ls` (indigo), in the same order.
- `car_expected_commit.txt`: the repo's DID and rev, from `goat repo inspect`.

HappyView's segment reader and DAG-CBOR conversion are tested against these, so a
bug shared by our reader and our writer can't pass unnoticed.
