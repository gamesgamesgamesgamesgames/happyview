import { describe, expect, test } from "bun:test";

import { assertValidNsid, InvalidNsidError, isValidNsid, nsidAuthority } from "../index";

describe("isValidNsid", () => {
  test("accepts digit-leading authority segments", () => {
    expect(isValidNsid("pics.2bit.feed.getPhotos")).toBe(true);
    expect(isValidNsid("pics.2-bit.photo")).toBe(true);
  });

  test("rejects position violations", () => {
    expect(isValidNsid("2bit.pics.photo")).toBe(false);
    expect(isValidNsid("pics.2bit.get-photos")).toBe(false);
    expect(isValidNsid("com.example")).toBe(false);
  });

  test("enforces the length cap", () => {
    expect(isValidNsid(`com.${"a".repeat(320)}.foo`)).toBe(false);
  });
});

describe("nsidAuthority", () => {
  test("reverses the authority segments", () => {
    expect(nsidAuthority("pics.2bit.feed.getPhotos")).toBe("feed.2bit.pics");
    expect(nsidAuthority("com.example.thing")).toBe("example.com");
  });

  test("validates before deriving", () => {
    expect(() => nsidAuthority("1.foo")).toThrow();
  });
});

describe("assertValidNsid", () => {
  test("names the offending NSID and links the spec", () => {
    expect(() => assertValidNsid("2bit.pics.photo")).toThrow(/2bit\.pics\.photo/);
    expect(() => assertValidNsid("2bit.pics.photo")).toThrow(/atproto\.com\/specs\/nsid/);
  });

  test("reports the length reason, not the segments reason, when over-length", () => {
    const long = `com.${"a".repeat(320)}.foo`;
    expect(() => assertValidNsid(long)).toThrow(/must be at most 317 characters/);
    try {
      assertValidNsid(long);
      throw new Error("expected assertValidNsid to throw");
    } catch (err) {
      expect(err).toBeInstanceOf(InvalidNsidError);
      expect((err as Error).message).not.toMatch(/dot-separated segments/);
    }
  });
});
