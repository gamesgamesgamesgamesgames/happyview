/**
 * AT Protocol NSID validation.
 *
 * The pattern below is byte-identical to `happyview_nsid::NSID_PATTERN` in
 * `crates/happyview-nsid/src/lib.rs`. Both are pinned by the same vendored
 * interop corpus.
 */

/**
 * The canonical NSID regex, copied verbatim from https://atproto.com/specs/nsid
 *
 * Exported as a string rather than a RegExp because JSON Schema `pattern`
 * fields take a string.
 *
 * Only the first (TLD) and last (name) segments must start with a letter; the
 * authority segments between them are reversed domain labels and may start with
 * a digit.
 */
export const NSID_PATTERN =
  "^[a-zA-Z]([a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?(\\.[a-zA-Z0-9]([a-zA-Z0-9-]{0,61}[a-zA-Z0-9])?)+(\\.[a-zA-Z][a-zA-Z0-9]{0,62})$";

/** The pattern bounds each segment but not the total, so this is separate. */
export const MAX_NSID_LEN = 317;

const NSID_RE = new RegExp(NSID_PATTERN);

export function isValidNsid(nsid: string): boolean {
  return nsid.length <= MAX_NSID_LEN && NSID_RE.test(nsid);
}

export class InvalidNsidError extends Error {
  constructor(public readonly nsid: string) {
    super(
      nsid.length > MAX_NSID_LEN
        ? `invalid NSID '${nsid}': must be at most ${MAX_NSID_LEN} characters`
        : `invalid NSID '${nsid}': must be at least three dot-separated segments, ` +
            `where only the first and last must start with a letter and the last ` +
            `takes no hyphens (see https://atproto.com/specs/nsid)`,
    );
    this.name = "InvalidNsidError";
  }
}

export function assertValidNsid(nsid: string): void {
  if (!isValidNsid(nsid)) throw new InvalidNsidError(nsid);
}

/**
 * Returns the NSID's authority: every segment except the name, reversed into a
 * domain. `pics.2bit.feed.getPhotos` yields `feed.2bit.pics`.
 */
export function nsidAuthority(nsid: string): string {
  assertValidNsid(nsid);
  return nsid.split(".").slice(0, -1).reverse().join(".");
}
