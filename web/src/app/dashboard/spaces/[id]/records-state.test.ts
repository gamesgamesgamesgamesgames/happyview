import { describe, expect, it } from "vitest";

import { ALL, stateForGrant } from "./records-state";

const cleared = {
  records: [],
  nextCursor: undefined,
  cursorStack: [],
  viewRecord: null,
};

describe("stateForGrant", () => {
  it("fixes an account grant's author to its target", () => {
    expect(
      stateForGrant(
        { scope: "account", target: "did:plc:alice" },
        "com.example.post",
      ),
    ).toEqual({
      ...cleared,
      filters: { collection: "com.example.post", repo: "did:plc:alice" },
    });
  });

  it("starts a space grant from every author", () => {
    expect(
      stateForGrant({ scope: "space", target: "space-1" }, "com.example.post"),
    ).toEqual({
      ...cleared,
      filters: { collection: "com.example.post", repo: ALL },
    });
  });

  it("clears the view and fetches nothing without a grant", () => {
    expect(stateForGrant(null, ALL)).toEqual({ ...cleared, filters: null });
  });
});
