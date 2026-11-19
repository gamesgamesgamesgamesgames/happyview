import type { SetupStatus } from "./api";

/** A step the setup wizard resumes at. */
export type ResumeStep =
  "mode" | "configure" | "attach-auth" | "verify" | "complete";

/**
 * Where the setup wizard picks up, from the setup status and the sign-in
 * payload the attach-auth step leaves while the admin signs in as the
 * attached account.
 *
 * Back from that sign-in, the admin's own session must be restored first.
 * An attached account counts as configured as soon as it's chosen, so the
 * pending sign-in is checked before the configured identity.
 */
export function resumeStep(
  status: SetupStatus,
  pendingAuth: string | null,
): { step: ResumeStep; attachedDid?: string } {
  if (status.setup_complete || status.plc_verified) return { step: "complete" };

  if (status.identity_mode === "attach_account" && pendingAuth) {
    try {
      const { attachedDid } = JSON.parse(pendingAuth) as {
        attachedDid?: unknown;
      };
      if (typeof attachedDid === "string" && attachedDid) {
        return { step: "attach-auth", attachedDid };
      }
    } catch {
      // An unreadable payload is no sign-in in progress.
    }
  }

  if (status.identity_configured) return { step: "verify" };
  if (status.identity_mode && status.identity_mode !== "not_exposed") {
    return { step: "configure" };
  }
  return { step: "mode" };
}
