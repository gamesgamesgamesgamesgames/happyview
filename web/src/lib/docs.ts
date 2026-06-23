const DOCS_BASE = (process.env.NEXT_PUBLIC_DOCS_URL || "/docs").replace(/\/$/, "")

export function docsUrl(path: string): string {
  const normalized = path.startsWith("/") ? path : `/${path}`
  return `${DOCS_BASE}${normalized}`
}
