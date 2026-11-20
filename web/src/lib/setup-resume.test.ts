import { describe, expect, it } from "vitest";

import type { SetupStatus } from "./api";
import { resumeStep } from "./setup-resume";

function status(overrides: Partial<SetupStatus>): SetupStatus {
  return {
    identity_mode: null,
    identity_configured: false,
    plc_verified: false,
    setup_complete: false,
    ...overrides,
  };
}

const pending = JSON.stringify({
  attachedDid: "did:plc:attached",
  originalDid: "did:plc:admin",
});

describe("resumeStep", () => {
  it("restores the admin's session first when back from the attached account's sign-in", () => {
    expect(
      resumeStep(
        status({ identity_mode: "attach_account", identity_configured: true }),
        pending,
      ),
    ).toEqual({ step: "attach-auth", attachedDid: "did:plc:attached" });
  });

  it("resumes an attached account at verify once the session is restored", () => {
    expect(
      resumeStep(
        status({ identity_mode: "attach_account", identity_configured: true }),
        null,
      ),
    ).toEqual({ step: "verify" });
  });

  it("ignores an unreadable sign-in payload", () => {
    expect(
      resumeStep(status({ identity_mode: "attach_account" }), "{not json"),
    ).toEqual({ step: "configure" });
  });

  it("ignores a sign-in payload in another mode", () => {
    expect(
      resumeStep(
        status({ identity_mode: "did_web", identity_configured: true }),
        pending,
      ),
    ).toEqual({ step: "verify" });
  });

  it("finishes a completed setup", () => {
    expect(
      resumeStep(
        status({ identity_mode: "attach_account", setup_complete: true }),
        pending,
      ),
    ).toEqual({ step: "complete" });
  });

  it("starts at the mode choice with no identity", () => {
    expect(resumeStep(status({}), null)).toEqual({ step: "mode" });
  });
});
