const DOCS_BASE = (
  process.env.NEXT_PUBLIC_DOCS_URL || "https://v2.happyview.dev"
).replace(/\/$/, "");

export function docsUrl(path: string): string {
  const normalized = path.startsWith("/") ? path : `/${path}`;
  return `${DOCS_BASE}${normalized}`;
}
