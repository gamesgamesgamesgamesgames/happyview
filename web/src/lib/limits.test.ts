import { describe, expect, it } from "vitest";

import { limitReachedText, limitRows } from "./limits";

const unlimited = {
  seats: { max: null, used: 2 },
  api_clients: { max: null, used: 4 },
  log_retention_days: { max: null, configured: 30 },
};

describe("limitRows", () => {
  it("shows nothing when the plan sets no limits", () => {
    expect(limitRows(unlimited)).toEqual([]);
  });

  it("shows each set limit with what is in use", () => {
    expect(
      limitRows({
        seats: { max: 5, used: 2 },
        api_clients: { max: 3, used: 3 },
        log_retention_days: { max: 7, configured: 3 },
      }),
    ).toEqual([
      { limit: "seats", label: "Seats", value: "2 of 5", over: false },
      {
        limit: "api_clients",
        label: "API clients",
        value: "3 of 3",
        over: false,
      },
      {
        limit: "log_retention_days",
        label: "Log retention",
        value: "Up to 7 days",
        over: false,
      },
    ]);
  });

  it("marks usage or a setting past the limit", () => {
    const rows = limitRows({
      seats: { max: 1, used: 3 },
      api_clients: { max: null, used: 9 },
      log_retention_days: { max: 7, configured: 0 },
    });
    expect(rows.map((r) => [r.limit, r.over])).toEqual([
      ["seats", true],
      ["log_retention_days", true],
    ]);
    expect(
      limitRows({
        ...unlimited,
        log_retention_days: { max: 1, configured: 30 },
      })[0].value,
    ).toBe("Up to 1 day");
  });
});

describe("limitReachedText", () => {
  it("names the limit with its plural", () => {
    expect(limitReachedText("seats", 1)).toBe(
      "This instance's plan allows 1 seat. Remove one, or move the instance to a larger plan.",
    );
    expect(limitReachedText("api_clients", 3)).toContain("3 API clients");
  });
});
