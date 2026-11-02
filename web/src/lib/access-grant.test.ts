import { describe, expect, it } from "vitest";

import type { AccessGrant } from "@/types/spaces";

import {
  formatGrantDuration,
  formatRemaining,
  grantDurations,
  grantForTarget,
  grantRequest,
} from "./access-grant";

const grant: AccessGrant = {
  id: "g1",
  user_id: "u1",
  user_did: "did:plc:moderator",
  scope: "space",
  target: "space-1",
  reason: "Report #1",
  created_at: "2026-11-01T00:00:00+00:00",
  expires_at: "2026-11-01T01:00:00+00:00",
  revoked_at: null,
  revoked_by: null,
};

describe("grantDurations", () => {
  it("offers nothing longer than the maximum, and the maximum itself", () => {
    expect(grantDurations(45)).toEqual([5, 15, 30, 45]);
    expect(grantDurations(60)).toEqual([5, 15, 30, 60]);
    expect(grantDurations(5)).toEqual([5]);
    expect(grantDurations(1440)).toEqual([5, 15, 30, 60, 120, 240, 480, 1440]);
  });
});

describe("formatGrantDuration", () => {
  it("names minutes below an hour and hours above", () => {
    expect(formatGrantDuration(15)).toBe("15 minutes");
    expect(formatGrantDuration(60)).toBe("1 hour");
    expect(formatGrantDuration(480)).toBe("8 hours");
  });
});

describe("grantRequest", () => {
  it("sends the chosen scope and target, the trimmed reason and a number", () => {
    expect(
      grantRequest(
        { scope: "account", target: "did:plc:author" },
        "  Report #2: spam \n",
        "30",
      ),
    ).toEqual({
      scope: "account",
      target: "did:plc:author",
      reason: "Report #2: spam",
      duration_minutes: 30,
    });
  });
});

describe("grantForTarget", () => {
  it("shows a grant only for the target it was fetched for", () => {
    const found = { key: "space-1", grant };
    expect(grantForTarget(found, "space-1", true)).toBe(grant);
    expect(grantForTarget(found, "space-2", true)).toBeNull();
  });

  it("shows nothing while the inspector is off", () => {
    expect(grantForTarget({ key: "space-1", grant }, "space-1", false)).toBe(
      null,
    );
    expect(grantForTarget(null, "space-1", true)).toBeNull();
  });
});

describe("formatRemaining", () => {
  it("counts down in minutes and seconds, never below zero", () => {
    expect(formatRemaining(61_500)).toBe("1:01 remaining");
    expect(formatRemaining(-5_000)).toBe("0:00 remaining");
  });
});
