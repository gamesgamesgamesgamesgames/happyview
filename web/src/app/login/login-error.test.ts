import { describe, expect, it } from "vitest";

import { loginErrorMessage } from "./login-error";

const message = (query: string) =>
  loginErrorMessage(new URLSearchParams(query));

describe("loginErrorMessage", () => {
  it("shows nothing without an error", () => {
    expect(message("")).toBeNull();
  });

  it("names the refused account by handle and DID", () => {
    expect(
      message("error=not_authorized&did=did%3Aplc%3Aabc&handle=alice.test"),
    ).toBe(
      "You signed in as @alice.test (did:plc:abc), which isn't a user of this dashboard. Sign in with another account, or ask an admin to add this one.",
    );
  });

  it("names it by DID alone when there is no handle", () => {
    expect(message("error=not_authorized&did=did%3Aplc%3Aabc")).toContain(
      "You signed in as did:plc:abc,",
    );
  });

  it("drops a handle that isn't one", () => {
    expect(
      message("error=not_authorized&did=did%3Aplc%3Aabc&handle=evil%20text"),
    ).toContain("You signed in as did:plc:abc,");
  });

  it("falls back to the plain message for a DID that isn't one", () => {
    expect(message("error=not_authorized&did=Click%20here")).toBe(
      "Your account is not authorized to access this dashboard.",
    );
  });

  it("adds the project hint only on a local instance", () => {
    expect(message("error=not_authorized&did=did%3Aplc%3Aabc")).not.toContain(
      "happyview dev",
    );
    expect(
      message("error=not_authorized&did=did%3Aplc%3Aabc&hint=local"),
    ).toContain("[instance] superadmin");
  });
});
