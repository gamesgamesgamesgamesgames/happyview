import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";

import { isValidNsid } from "../index";

// The corpus deliberately contains leading- and trailing-whitespace cases:
// `one.two.three ` and ` one.two.three` are both listed invalid, while the bare
// form is valid. Calling .trim() here silently turns both into passes. Split on
// newlines and touch nothing else.
function cases(file: string): string[] {
  const raw = readFileSync(join(import.meta.dir, "../../tests/interop", file), "utf8");
  return raw
    .split("\n")
    .map((line) => (line.endsWith("\r") ? line.slice(0, -1) : line))
    .filter((line) => line !== "" && !line.startsWith("#"));
}

describe("atproto interop corpus", () => {
  test("accepts every valid case", () => {
    const valid = cases("nsid_syntax_valid.txt");
    expect(valid.length).toBe(25);
    for (const nsid of valid) {
      expect(isValidNsid(nsid)).toBe(true);
    }
  });

  test("rejects every invalid case", () => {
    const invalid = cases("nsid_syntax_invalid.txt");
    expect(invalid.length).toBe(27);
    for (const nsid of invalid) {
      expect(isValidNsid(nsid)).toBe(false);
    }
  });

  test("the vendored corpus matches the Rust crate's copy byte-for-byte", () => {
    for (const file of ["nsid_syntax_valid.txt", "nsid_syntax_invalid.txt"]) {
      const ts = readFileSync(join(import.meta.dir, "../../tests/interop", file));
      const rs = readFileSync(
        join(import.meta.dir, "../../../../crates/happyview-nsid/tests/interop", file),
      );
      expect(ts.equals(rs)).toBe(true);
    }
  });
});
