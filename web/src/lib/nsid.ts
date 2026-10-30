export function nsidToDomain(nsid: string): string | null {
  const parts = nsid.split(".");
  if (parts.length < 3) return null;
  const authority = parts.slice(0, -1).reverse();
  return authority.join(".");
}

export interface ResolvedNsid {
  type: string | undefined;
  lexiconJson: Record<string, unknown> | undefined;
}

export async function resolveNsid(
  nsid: string,
  signal: AbortSignal,
): Promise<ResolvedNsid> {
  const empty: ResolvedNsid = { type: undefined, lexiconJson: undefined };
  const domain = nsidToDomain(nsid);
  if (!domain) return empty;

  let did: string | undefined;
  try {
    const resp = await fetch(`https://${domain}/.well-known/atproto-did`, {
      signal,
    });
    if (resp.ok) did = (await resp.text()).trim();
  } catch (e) {
    if (signal.aborted) return empty;
  }

  if (!did) {
    try {
      const resp = await fetch(
        `https://bsky.social/xrpc/com.atproto.identity.resolveHandle?handle=${encodeURIComponent(domain)}`,
        { signal },
      );
      if (resp.ok) {
        const data = await resp.json();
        did = data.did;
      }
    } catch (e) {
      if (signal.aborted) return empty;
    }
  }

  if (!did) return empty;

  let pdsEndpoint: string | undefined;
  try {
    const resp = await fetch(
      `https://plc.directory/${encodeURIComponent(did)}`,
      { signal },
    );
    if (resp.ok) {
      const doc = await resp.json();
      const services = doc.service as
        { id: string; serviceEndpoint: string }[] | undefined;
      pdsEndpoint = services?.find(
        (s) => s.id === "#atproto_pds",
      )?.serviceEndpoint;
    }
  } catch (e) {
    if (signal.aborted) return empty;
  }

  if (!pdsEndpoint) return empty;

  try {
    const resp = await fetch(
      `${pdsEndpoint}/xrpc/com.atproto.repo.getRecord?repo=${encodeURIComponent(did)}&collection=com.atproto.lexicon.schema&rkey=${encodeURIComponent(nsid)}`,
      { signal },
    );
    if (resp.ok) {
      const data = await resp.json();
      const value = data.value as Record<string, unknown> | undefined;
      const mainType = (
        value?.defs as Record<string, Record<string, unknown>> | undefined
      )?.main?.type as string | undefined;
      return { type: mainType, lexiconJson: value };
    }
  } catch {
    // Best-effort resolution
  }

  return empty;
}
