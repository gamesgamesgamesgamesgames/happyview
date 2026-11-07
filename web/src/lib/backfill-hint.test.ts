import { beforeEach, describe, expect, it, vi } from "vitest";

import { getLexiconRecordCount } from "./api";
import {
  addedCollection,
  backfillHref,
  suggestBackfill,
  withAddedScript,
} from "./backfill-hint";

vi.mock("./api", () => ({ getLexiconRecordCount: vi.fn() }));

const recordCount = vi.mocked(getLexiconRecordCount);

function countIs(count: number | null) {
  recordCount.mockResolvedValue({
    collection: "com.example.post",
    count,
    deletable: true,
    not_deletable_reason: null,
  });
}

describe("withAddedScript", () => {
  it("carries a record script's collection", () => {
    for (const kind of [
      "record.index",
      "record.create",
      "record.update",
      "record.delete",
    ]) {
      expect(
        withAddedScript(
          "/dashboard/settings/scripts/x",
          `${kind}:com.example.post`,
        ),
      ).toBe("/dashboard/settings/scripts/x?added=com.example.post");
    }
  });

  it("adds to a destination that already has a query", () => {
    expect(
      withAddedScript("/a?lexicon=b", "record.create:com.example.post"),
    ).toBe("/a?lexicon=b&added=com.example.post");
  });

  it("leaves any other script's destination alone", () => {
    for (const id of [
      "xrpc.query:com.example.getPosts",
      "xrpc.procedure:com.example.createPost",
      "job.run:nightly",
      "labeler.apply:_actor",
      "labeler.apply:com.example.post",
      "not a trigger",
    ]) {
      expect(addedCollection(id)).toBeNull();
      expect(withAddedScript("/dashboard/lexicons/x", id)).toBe(
        "/dashboard/lexicons/x",
      );
    }
  });
});

describe("suggestBackfill", () => {
  // A block body: a function `beforeEach` returns is run as its teardown.
  beforeEach(() => {
    recordCount.mockReset();
  });

  it("suggests one when the collection has records", async () => {
    countIs(12);
    expect(await suggestBackfill("com.example.post")).toBe(true);
    expect(recordCount).toHaveBeenCalledWith("com.example.post");
  });

  it("suggests one when the count is unknown", async () => {
    countIs(null);
    expect(await suggestBackfill("com.example.post")).toBe(true);
  });

  it("suggests one when the count can't be fetched", async () => {
    recordCount.mockRejectedValue(new Error("nope"));
    expect(await suggestBackfill("com.example.post")).toBe(true);
  });

  it("doesn't when the collection has no records", async () => {
    countIs(0);
    expect(await suggestBackfill("com.example.post")).toBe(false);
  });
});

describe("backfillHref", () => {
  it("opens the form on the collection", () => {
    expect(backfillHref("com.example.post")).toBe(
      "/dashboard/backfill?collection=com.example.post",
    );
  });
});
