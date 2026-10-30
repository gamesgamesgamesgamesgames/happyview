import { describe, expect, it } from "vitest";

import { classifyConfigFailure, retryDelayMs } from "./config-load";

describe("classifyConfigFailure", () => {
  it("reads a 421 as a host the server does not serve", () => {
    expect(
      classifyConfigFailure(421, "app.example.com", "Unknown host"),
    ).toEqual({
      kind: "unknown-host",
      host: "app.example.com",
      isLocalhost: false,
    });
  });

  it("notices localhost, with or without a port", () => {
    for (const host of ["localhost", "localhost:3000", "LOCALHOST:3000"]) {
      const failure = classifyConfigFailure(421, host, "");
      expect(failure.kind === "unknown-host" && failure.isLocalhost).toBe(true);
    }
  });

  it("reads anything else as an instance it cannot reach", () => {
    for (const status of [null, 500, 502, 503, 404]) {
      expect(classifyConfigFailure(status, "localhost", "boom").kind).toBe(
        "unreachable",
      );
    }
  });
});

describe("retryDelayMs", () => {
  it("doubles from one second and stops at thirty", () => {
    expect([0, 1, 2, 3, 4, 5, 6, 20].map(retryDelayMs)).toEqual([
      1_000, 2_000, 4_000, 8_000, 16_000, 30_000, 30_000, 30_000,
    ]);
  });
});
