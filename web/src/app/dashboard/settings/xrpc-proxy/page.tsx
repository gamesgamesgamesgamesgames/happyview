"use client"

import { useCallback, useEffect, useState } from "react"
import { X } from "lucide-react"

import { useCurrentUser } from "@/hooks/use-current-user"
import {
  getProxyConfig,
  updateProxyConfig,
  type ProxyConfig,
  type ProxyRouting,
} from "@/lib/api"
import { SiteHeader } from "@/components/site-header"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"

const MODES = [
  {
    value: "disabled" as const,
    label: "Disabled",
    description: "Block all proxy requests. Only locally registered lexicons are served.",
  },
  {
    value: "open" as const,
    label: "Open",
    description:
      "Proxy all unrecognized NSIDs to their resolved authority. This is the default.",
  },
  {
    value: "allowlist" as const,
    label: "Allowlist",
    description:
      "Only proxy NSIDs that match a pattern below. Everything else returns 403.",
  },
  {
    value: "blocklist" as const,
    label: "Blocklist",
    description:
      "Proxy all NSIDs except those that match a pattern below.",
  },
]

const ROUTINGS = [
  {
    value: "authority" as const,
    label: "Lexicon authority",
    description:
      "Resolve the NSID's authority via DNS and forward there, unauthenticated. This is the current default.",
    caveat:
      "This answers \u201cwho defines this schema\u201d, not \u201cwho serves this request\u201d. Repo methods such as com.atproto.repo.createRecord resolve to whoever publishes the com.atproto lexicons, which has no access to your users\u2019 repos \u2014 those requests arrive unauthenticated and are refused.",
  },
  {
    value: "serviceproxy" as const,
    label: "Service proxy",
    description:
      "Forward to the caller's own PDS, authenticated as them, per the AT Protocol service proxying model.",
    caveat:
      "Requires authentication: an unrecognized request from an anonymous caller returns 401, because the destination depends on who is asking. Requests carrying an atproto-proxy header are passed through for the PDS to relay onwards.",
  },
]

export default function XrpcProxySettingsPage() {
  const { hasPermission } = useCurrentUser()
  const canManage = hasPermission("settings:manage")

  const [mode, setMode] = useState<ProxyConfig["mode"]>("open")
  const [routing, setRouting] = useState<ProxyRouting>("authority")
  const [nsids, setNsids] = useState<string[]>([""])
  const [error, setError] = useState<string | null>(null)
  const [saving, setSaving] = useState(false)
  const [notice, setNotice] = useState<string | null>(null)

  const load = useCallback(async () => {
    try {
      const config = await getProxyConfig()
      setMode(config.mode)
      setRouting(config.routing ?? "authority")
      setNsids(config.nsids.length > 0 ? [...config.nsids, ""] : [""])
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e))
    }
  }, [])

  useEffect(() => {
    load()
  }, [load])

  const showNsids = mode === "allowlist" || mode === "blocklist"

  async function handleSave() {
    setError(null)
    setNotice(null)
    setSaving(true)
    try {
      const filteredNsids = nsids.map((s) => s.trim()).filter(Boolean)
      await updateProxyConfig({ mode, nsids: filteredNsids, routing })
      setNotice("Proxy settings saved.")
      await load()
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setSaving(false)
    }
  }

  function handleNsidChange(index: number, value: string) {
    const next = [...nsids]
    next[index] = value
    if (index === nsids.length - 1 && value.trim() !== "") {
      next.push("")
    }
    setNsids(next)
  }

  function handleNsidRemove(index: number) {
    const next = nsids.filter((_, i) => i !== index)
    if (next.length === 0 || next[next.length - 1].trim() !== "") {
      next.push("")
    }
    setNsids(next)
  }

  function handleNsidPaste(
    index: number,
    e: React.ClipboardEvent<HTMLInputElement>,
  ) {
    const text = e.clipboardData.getData("text")
    const parts = text.split(/[,;\s\n]+/).map((s) => s.trim()).filter(Boolean)
    if (parts.length <= 1) return
    e.preventDefault()
    const before = nsids.slice(0, index)
    const after = nsids.slice(index + 1).filter((s) => s.trim() !== "")
    const next = [...before, ...parts, ...after, ""]
    setNsids(next)
  }

  function handleNsidKeyDown(
    index: number,
    e: React.KeyboardEvent<HTMLInputElement>,
  ) {
    if (e.key === "Backspace" && nsids[index] === "" && nsids.length > 1) {
      e.preventDefault()
      handleNsidRemove(index)
    }
  }

  return (
    <>
      <SiteHeader title="XRPC Proxy" />
      <div className="flex flex-1 flex-col gap-6 p-4 md:p-6 max-w-3xl">
        {error && <p className="text-destructive text-sm">{error}</p>}
        {notice && (
          <p className="text-sm text-green-600 dark:text-green-400">
            {notice}
          </p>
        )}

        <div>
          <h2 className="text-lg font-semibold">Proxy Mode</h2>
          <p className="text-muted-foreground text-sm">
            Control which unrecognized XRPC methods are forwarded to their
            resolved authority. Locally registered lexicons are always served
            regardless of this setting.
          </p>
        </div>

        <fieldset className="flex flex-col gap-3" disabled={!canManage}>
          {MODES.map((m) => (
            <label
              key={m.value}
              className="flex items-start gap-3 rounded-lg border p-3 cursor-pointer has-[:checked]:border-primary has-[:checked]:bg-primary/5"
            >
              <input
                type="radio"
                name="proxy-mode"
                value={m.value}
                checked={mode === m.value}
                onChange={() => setMode(m.value)}
                className="mt-1"
              />
              <div>
                <span className="font-medium text-sm">{m.label}</span>
                <p className="text-muted-foreground text-xs">{m.description}</p>
              </div>
            </label>
          ))}
        </fieldset>

        <div>
          <h2 className="text-lg font-semibold">Routing</h2>
          <p className="text-muted-foreground text-sm">
            Where a proxied request is sent. Independent of the mode above,
            which only decides whether a method may be proxied at all.
          </p>
        </div>

        <fieldset className="flex flex-col gap-3" disabled={!canManage}>
          {ROUTINGS.map((r) => (
            <label
              key={r.value}
              className="flex items-start gap-3 rounded-lg border p-3 cursor-pointer has-[:checked]:border-primary has-[:checked]:bg-primary/5"
            >
              <input
                type="radio"
                name="proxy-routing"
                value={r.value}
                checked={routing === r.value}
                onChange={() => setRouting(r.value)}
                className="mt-1"
              />
              <div>
                <span className="font-medium text-sm">{r.label}</span>
                <p className="text-muted-foreground text-xs">{r.description}</p>
                <p className="text-muted-foreground text-xs mt-1.5">
                  {r.caveat}
                </p>
              </div>
            </label>
          ))}
        </fieldset>

        {showNsids && (
          <div className="flex flex-col gap-2">
            <Label>
              NSID Patterns
            </Label>
            <p className="text-muted-foreground text-xs">
              Enter NSID patterns. Use <code className="text-[11px] bg-muted px-1 py-0.5 rounded">com.example.*</code> to
              match all NSIDs under a namespace.
            </p>
            <div className="flex flex-col gap-1.5">
              {nsids.map((val, index) => (
                <div key={index} className="flex gap-1.5">
                  <Input
                    value={val}
                    onChange={(e) => handleNsidChange(index, e.target.value)}
                    onKeyDown={(e) => handleNsidKeyDown(index, e)}
                    onPaste={(e) => handleNsidPaste(index, e)}
                    placeholder="com.example.feed.*"
                    className="font-mono text-sm"
                    disabled={!canManage}
                  />
                  {nsids.length > 1 && val !== "" && (
                    <Button
                      type="button"
                      variant="ghost"
                      size="icon"
                      onClick={() => handleNsidRemove(index)}
                      disabled={!canManage}
                    >
                      <X className="size-4" />
                    </Button>
                  )}
                </div>
              ))}
            </div>
          </div>
        )}

        <div className="flex justify-end pt-2">
          <Button onClick={handleSave} disabled={!canManage || saving}>
            {saving ? "Saving..." : "Save changes"}
          </Button>
        </div>
      </div>
    </>
  )
}
