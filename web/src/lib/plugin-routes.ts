/**
 * The id a pasted manifest URL is reached under, so that a URL install lands
 * on the same detail page as a catalogue entry. A plugin id is NSID-shaped, so
 * a leading underscore cannot collide with one.
 */
export const URL_PLUGIN_ID = "_url"

export function installedPluginHref(id: string): string {
  return `/dashboard/settings/plugins/${encodeURIComponent(id)}`
}

export function cataloguePluginHref(id: string): string {
  return `/dashboard/settings/plugins/add/${encodeURIComponent(id)}`
}

export function urlPluginHref(manifestUrl: string): string {
  return `${cataloguePluginHref(URL_PLUGIN_ID)}?manifest=${encodeURIComponent(manifestUrl)}`
}
