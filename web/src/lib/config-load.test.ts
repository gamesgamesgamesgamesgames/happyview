import { describe, expect, it, vi } from "vitest";

import {
  classifyConfigFailure,
  parseFeatures,
  refreshFrom,
  retryDelayMs,
} from "./config-load";

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

describe("parseFeatures", () => {
  it("reads each feature the server turns on", () => {
    expect(parseFeatures({ spaces: true, space_inspector: true })).toEqual({
      spaces: true,
      space_inspector: true,
    });
  });

  it("reads anything but true as off, including a server without the field", () => {
    expect(parseFeatures({ spaces: "true", space_inspector: 1 })).toEqual({
      spaces: false,
      space_inspector: false,
    });
    expect(parseFeatures({ spaces: true })).toEqual({
      spaces: true,
      space_inspector: false,
    });
    expect(parseFeatures(undefined)).toEqual({
      spaces: false,
      space_inspector: false,
    });
  });
});

describe("refreshFrom", () => {
  it("keeps the loaded config when a refresh cannot reach the server", async () => {
    const set = vi.fn();
    await expect(
      refreshFrom(
        async () => ({
          failure: { kind: "unreachable", detail: "The server answered 502." },
        }),
        set,
      ),
    ).rejects.toThrow("The server answered 502.");
    expect(set).not.toHaveBeenCalled();
  });

  it("keeps the loaded config when the server does not serve the host", async () => {
    const set = vi.fn();
    await expect(
      refreshFrom(
        async () => ({
          failure: {
            kind: "unknown-host",
            host: "app.example.com",
            isLocalhost: false,
          },
        }),
        set,
      ),
    ).rejects.toThrow("The server does not serve app.example.com.");
    expect(set).not.toHaveBeenCalled();
  });

  it("hands a loaded config to set once", async () => {
    const set = vi.fn();
    const config = { public_url: "https://example.com" };
    await refreshFrom(async () => ({ config }), set);
    expect(set).toHaveBeenCalledTimes(1);
    expect(set).toHaveBeenCalledWith(config);
  });
});
