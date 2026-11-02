import { describe, expect, it } from "vitest";

import { limitToast, parseLimitReached } from "./limits";

describe("parseLimitReached", () => {
  it("reads a refusal with the host's wording and action", () => {
    expect(
      parseLimitReached({
        error: "LimitReached",
        limit: "seats",
        max: 5,
        current: 5,
        message: "Your plan includes 5 seats.",
        actionUrl: "https://host.example/plan",
        actionLabel: "Add a seat",
      }),
    ).toEqual({
      limit: "seats",
      max: 5,
      current: 5,
      message: "Your plan includes 5 seats.",
      actionUrl: "https://host.example/plan",
      actionLabel: "Add a seat",
    });
  });

  it("reads a gate, which has no counts", () => {
    expect(
      parseLimitReached({
        error: "LimitReached",
        limit: "anonymous_xrpc",
        message:
          "This instance only answers XRPC calls made with an API client.",
      }),
    ).toEqual({
      limit: "anonymous_xrpc",
      message: "This instance only answers XRPC calls made with an API client.",
    });
  });

  it("drops an action missing a half, or one that isn't a web URL", () => {
    const base = { error: "LimitReached", limit: "seats", message: "m" };
    for (const extra of [
      { actionUrl: "https://host.example/plan" },
      { actionLabel: "Add a seat" },
      { actionUrl: "javascript:alert(1)", actionLabel: "Add a seat" },
      { actionUrl: "not a url", actionLabel: "Add a seat" },
    ]) {
      const reached = parseLimitReached({ ...base, ...extra });
      expect(reached?.actionUrl).toBeUndefined();
      expect(reached?.actionLabel).toBeUndefined();
    }
  });

  it("has something to say when the message is missing", () => {
    expect(
      parseLimitReached({ error: "LimitReached", limit: "seats" })?.message,
    ).toBe("This instance's limits don't allow that.");
  });

  it("is undefined for any other error", () => {
    expect(parseLimitReached({ error: "Forbidden", message: "no" })).toBe(
      undefined,
    );
    expect(parseLimitReached({ error: "LimitReached" })).toBe(undefined);
    expect(parseLimitReached("LimitReached")).toBe(undefined);
    expect(parseLimitReached(null)).toBe(undefined);
  });
});

describe("limitToast", () => {
  it("says what was refused, in the host's words, with its action", () => {
    expect(
      limitToast("Add user", {
        limit: "seats",
        message: "Your plan includes 1 seat.",
        actionUrl: "https://host.example/plan",
        actionLabel: "Add a seat",
      }),
    ).toEqual({
      title: "Add user: limit reached",
      description: "Your plan includes 1 seat.",
      action: { label: "Add a seat", url: "https://host.example/plan" },
    });
    expect(
      limitToast("Add user", { limit: "seats", message: "m" }).action,
    ).toBe(undefined);
  });
});
