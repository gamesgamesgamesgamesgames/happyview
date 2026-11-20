import { completeSetup, plcCheck, plcSubmit } from "./api";

/** Where the attached account's PLC step stands after an attempt. */
export type AttachPlcOutcome =
  | { done: true }
  | {
      done: false;
      error: string;
      /**
       * The submit failed, so the PDS may still have applied the operation.
       * Checking the DID document can finish the step.
       */
      canCheck: boolean;
    };

function message(e: unknown, fallback: string): string {
  return e instanceof Error ? e.message : fallback;
}

async function complete(): Promise<AttachPlcOutcome> {
  try {
    await completeSetup();
    return { done: true };
  } catch (e) {
    return {
      done: false,
      error: message(e, "Couldn't finish setup. Try again."),
      canCheck: false,
    };
  }
}

/** Submit the confirmation code, then finish setup. */
export async function submitAttachedPlc(
  token: string,
): Promise<AttachPlcOutcome> {
  try {
    await plcSubmit(token);
  } catch (e) {
    return {
      done: false,
      error: message(e, "Verification failed. Check the code and try again."),
      canCheck: true,
    };
  }
  return complete();
}

/** Finish setup if the DID document already lists this instance. */
export async function checkAttachedPlc(): Promise<AttachPlcOutcome> {
  try {
    await plcCheck();
  } catch (e) {
    return {
      done: false,
      error: message(e, "Couldn't check the DID document. Try again."),
      canCheck: true,
    };
  }
  return complete();
}
