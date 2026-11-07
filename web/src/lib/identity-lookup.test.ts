import { beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("./api", () => {
  class ApiError extends Error {
    status: number;
    constructor(status: number, message: string) {
      super(message);
      this.status = status;
    }
  }
  return { ApiError, resolveIdentity: vi.fn() };
});

import { ApiError, resolveIdentity } from "./api";
import {
  MAX_IN_FLIGHT,
  lookupIdentity,
  resetIdentityLookups,
} from "./identity-lookup";

const resolve = vi.mocked(resolveIdentity);

const identity = {
  did: "did:plc:alice",
  handle: "alice.test",
} as unknown as Awaited<ReturnType<typeof resolveIdentity>>;

beforeEach(() => {
  resetIdentityLookups();
  resolve.mockReset();
});

describe("lookupIdentity", () => {
  it("caches a resolved identity", async () => {
    resolve.mockResolvedValue(identity);
    expect(await lookupIdentity("did:plc:alice")).toBe(identity);
    expect(await lookupIdentity("did:plc:alice")).toBe(identity);
    expect(resolve).toHaveBeenCalledTimes(1);
    expect(resolve).toHaveBeenCalledWith("did:plc:alice", { profile: true });
  });

  it("caches a 400 as an unresolvable DID", async () => {
    resolve.mockRejectedValue(new ApiError(400, "Invalid DID"));
    expect(await lookupIdentity("did:plc:bad")).toBeNull();
    expect(await lookupIdentity("did:plc:bad")).toBeNull();
    expect(resolve).toHaveBeenCalledTimes(1);
  });

  it("retries after a 502, which says nothing about the DID", async () => {
    resolve
      .mockRejectedValueOnce(new ApiError(502, "Upstream timed out"))
      .mockResolvedValueOnce(identity);
    expect(await lookupIdentity("did:plc:alice")).toBe("unavailable");
    expect(await lookupIdentity("did:plc:alice")).toBe(identity);
    expect(resolve).toHaveBeenCalledTimes(2);
  });

  it("retries after a network error", async () => {
    resolve
      .mockRejectedValueOnce(new TypeError("Failed to fetch"))
      .mockResolvedValueOnce(identity);
    expect(await lookupIdentity("did:plc:alice")).toBe("unavailable");
    expect(await lookupIdentity("did:plc:alice")).toBe(identity);
    expect(resolve).toHaveBeenCalledTimes(2);
  });

  it(`runs at most ${MAX_IN_FLIGHT} lookups at once`, async () => {
    const answers: Array<() => void> = [];
    resolve.mockImplementation(
      () =>
        new Promise((done) => {
          answers.push(() => done(identity));
        }),
    );
    const flush = () => new Promise((r) => setTimeout(r, 0));

    const lookups = Array.from({ length: MAX_IN_FLIGHT + 2 }, (_, i) =>
      lookupIdentity(`did:plc:${i}`),
    );
    await flush();
    expect(resolve).toHaveBeenCalledTimes(MAX_IN_FLIGHT);

    answers[0]();
    await flush();
    expect(resolve).toHaveBeenCalledTimes(MAX_IN_FLIGHT + 1);

    for (let i = 1; i < answers.length; i++) answers[i]();
    await flush();
    answers[answers.length - 1]();
    await Promise.all(lookups);
    expect(resolve).toHaveBeenCalledTimes(MAX_IN_FLIGHT + 2);
  });
});
