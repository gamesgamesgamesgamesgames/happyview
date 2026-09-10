import { describe, expect, test } from "bun:test";

import { isClientAssertionRequired } from "../browser-client";

describe("isClientAssertionRequired", () => {
  const atprotoError =
    '{"error":"invalid_request","error_description":"client authentication method \\"private_key_jwt\\" required a \\"client_assertion\\""}';

  test("true for the atproto assertion-required error at 400", () => {
    expect(isClientAssertionRequired(400, atprotoError)).toBe(true);
  });

  test("true at 401 as well, since servers differ on which they use", () => {
    expect(isClientAssertionRequired(401, atprotoError)).toBe(true);
  });

  test("false for a DPoP-nonce challenge, so it does not steal the nonce retry", () => {
    expect(isClientAssertionRequired(400, '{"error":"use_dpop_nonce"}')).toBe(
      false,
    );
  });

  test("false for an unrelated 400 such as invalid_grant", () => {
    expect(isClientAssertionRequired(400, '{"error":"invalid_grant"}')).toBe(
      false,
    );
  });

  test("false for any non-4xx-auth status, even if the word appears", () => {
    expect(isClientAssertionRequired(200, "client_assertion")).toBe(false);
    expect(isClientAssertionRequired(500, atprotoError)).toBe(false);
  });
});
