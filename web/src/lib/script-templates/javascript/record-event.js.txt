import log from "internal.logging";

export default function handle(input, ctx) {
  if (input.action === "delete") {
    log.info("record deleted", { uri: input.uri });
  } else {
    log.info(`record ${input.action}`, { uri: input.uri, did: input.did });
  }

  // Returning nothing skips the operation, and a delete carries no record,
  // so `true` is what lets a delete through.
  return input.record ?? true;
}
