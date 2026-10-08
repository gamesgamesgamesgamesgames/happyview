/** CIDs of the blob refs anywhere in a record. */
export function blobCids(value: unknown, found: string[] = []): string[] {
  if (Array.isArray(value)) {
    for (const item of value) blobCids(item, found);
  } else if (value && typeof value === "object") {
    const obj = value as Record<string, unknown>;
    const ref = obj.ref as Record<string, unknown> | undefined;
    if (obj.$type === "blob" && typeof ref?.$link === "string") {
      found.push(ref.$link);
    }
    for (const child of Object.values(obj)) blobCids(child, found);
  }
  return found;
}
