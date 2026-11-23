import { beforeEach, describe, expect, it, vi } from "vitest";

import { completeSetup, plcCheck, plcSubmit } from "./api";
import { checkAttachedPlc, submitAttachedPlc } from "./attach-plc";

vi.mock("./api", () => ({
  completeSetup: vi.fn(),
  plcCheck: vi.fn(),
  plcSubmit: vi.fn(),
}));

const submit = vi.mocked(plcSubmit);
const check = vi.mocked(plcCheck);
const complete = vi.mocked(completeSetup);

beforeEach(() => {
  vi.resetAllMocks();
});

describe("submitAttachedPlc", () => {
  it("submits the code, then completes setup", async () => {
    const calls: string[] = [];
    submit.mockImplementation(async () => {
      calls.push("submit");
    });
    complete.mockImplementation(async () => {
      calls.push("complete");
    });

    expect(await submitAttachedPlc("code")).toEqual({ done: true });
    expect(submit).toHaveBeenCalledWith("code");
    expect(calls).toEqual(["submit", "complete"]);
  });

  it("offers the DID document check when the submit fails", async () => {
    submit.mockRejectedValue(new Error("submitPlcOperation failed"));

    expect(await submitAttachedPlc("code")).toEqual({
      done: false,
      error: "submitPlcOperation failed",
      canCheck: true,
    });
    expect(complete).not.toHaveBeenCalled();
  });

  it("doesn't offer the check when only completing fails", async () => {
    complete.mockRejectedValue(new Error("setup already complete"));

    expect(await submitAttachedPlc("code")).toEqual({
      done: false,
      error: "setup already complete",
      canCheck: false,
    });
  });
});

describe("checkAttachedPlc", () => {
  it("completes setup once the DID document lists the instance", async () => {
    expect(await checkAttachedPlc()).toEqual({ done: true });
    expect(check).toHaveBeenCalled();
    expect(complete).toHaveBeenCalled();
  });

  it("keeps the step open when the DID document doesn't list it", async () => {
    check.mockRejectedValue(
      new Error("The DID document doesn't list this instance yet."),
    );

    expect(await checkAttachedPlc()).toEqual({
      done: false,
      error: "The DID document doesn't list this instance yet.",
      canCheck: true,
    });
    expect(complete).not.toHaveBeenCalled();
  });
});
