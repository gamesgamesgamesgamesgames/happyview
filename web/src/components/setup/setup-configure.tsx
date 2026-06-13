"use client"

import { useCallback, useEffect, useRef, useState } from "react"
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Avatar, AvatarFallback, AvatarImage } from "@/components/ui/avatar"
import { setSetupIdentity, resolveIdentity, type ResolveResult } from "@/lib/api"

interface SetupConfigureProps {
  mode: string
  onComplete: (opts?: { attachedDid?: string; attachedHandle?: string | null }) => void
}

export function SetupConfigure({ mode, onComplete }: SetupConfigureProps) {
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [identifier, setIdentifier] = useState("")

  const handleSubmit = async () => {
    setLoading(true)
    setError(null)
    try {
      await setSetupIdentity({
        mode,
        ...(mode === "attach_account" ? { attached_account_did: identifier } : {}),
      })
      onComplete()
    } catch (e) {
      setError(e instanceof Error ? e.message : "Setup failed")
    } finally {
      setLoading(false)
    }
  }

  if (mode === "did_web") {
    return (
      <Card>
        <CardHeader><CardTitle>Configure did:web</CardTitle>
          <CardDescription>HappyView will serve a DID document at <code className="bg-muted px-1.5 py-0.5 rounded text-sm">/.well-known/did.json</code> using your instance domain.</CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          <div><Label>Signing Key</Label><p className="text-muted-foreground text-sm mt-1">A new P-256 keypair will be generated and encrypted at rest.</p></div>
          <p className="text-muted-foreground text-sm">You can add service entries after setup from the Service Identity settings page.</p>
          {error && <p className="text-destructive text-sm">{error}</p>}
          <div className="flex justify-end"><Button onClick={handleSubmit} disabled={loading}>{loading ? "Configuring..." : "Continue"}</Button></div>
        </CardContent>
      </Card>
    )
  }

  if (mode === "attach_account") {
    return (
      <AttachAccountForm onComplete={(opts) => onComplete(opts)} />
    )
  }

  if (mode === "did_plc") {
    return (
      <Card>
        <CardHeader><CardTitle>Create did:plc</CardTitle>
          <CardDescription>A new DID will be registered in the PLC directory. HappyView will generate and manage the signing and rotation keypairs.</CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          <div><Label>Signing Key</Label><p className="text-muted-foreground text-sm mt-1">A new P-256 keypair will be generated and encrypted at rest.</p></div>
          <div><Label>Rotation Key</Label><p className="text-muted-foreground text-sm mt-1">A separate rotation key will be generated. You'll be able to export it in the next step.</p></div>
          {error && <p className="text-destructive text-sm">{error}</p>}
          <div className="flex justify-end"><Button onClick={handleSubmit} disabled={loading}>{loading ? "Configuring..." : "Continue"}</Button></div>
        </CardContent>
      </Card>
    )
  }

  return null
}

function AttachAccountForm({ onComplete }: { onComplete: (opts: { attachedDid: string; attachedHandle: string | null }) => void }) {
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [inputValue, setInputValue] = useState("")
  const [suggestions, setSuggestions] = useState<ResolveResult[]>([])
  const [showSuggestions, setShowSuggestions] = useState(false)
  const [selectedProfile, setSelectedProfile] = useState<ResolveResult | null>(null)
  const [resolving, setResolving] = useState(false)
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  const containerRef = useRef<HTMLDivElement>(null)

  // Close suggestions on outside click
  useEffect(() => {
    function handleClickOutside(e: MouseEvent) {
      if (containerRef.current && !containerRef.current.contains(e.target as Node)) {
        setShowSuggestions(false)
      }
    }
    document.addEventListener("mousedown", handleClickOutside)
    return () => document.removeEventListener("mousedown", handleClickOutside)
  }, [])

  const searchIdentity = useCallback(async (query: string) => {
    const q = query.trim()
    if (q.length < 2) {
      setSuggestions([])
      setShowSuggestions(false)
      return
    }

    setResolving(true)
    try {
      const results = await resolveIdentity(q)
      setSuggestions(results)
      setShowSuggestions(results.length > 0)
    } catch {
      // Silently fail — typeahead is optional
      setSuggestions([])
    } finally {
      setResolving(false)
    }
  }, [])

  function handleInputChange(value: string) {
    setInputValue(value)
    setSelectedProfile(null)

    if (debounceRef.current) clearTimeout(debounceRef.current)
    debounceRef.current = setTimeout(() => searchIdentity(value), 300)
  }

  function selectResult(result: ResolveResult) {
    setSelectedProfile(result)
    setInputValue(result.handle ?? result.did)
    setShowSuggestions(false)
    setSuggestions([])
  }

  function clearSelection() {
    setSelectedProfile(null)
    setInputValue("")
    setSuggestions([])
  }

  async function handleSubmit() {
    const did = selectedProfile?.did ?? inputValue.trim()
    if (!did) return

    setLoading(true)
    setError(null)
    try {
      await setSetupIdentity({ mode: "attach_account", attached_account_did: did })
      onComplete({
        attachedDid: did,
        attachedHandle: selectedProfile?.handle ?? null,
      })
    } catch (e) {
      setError(e instanceof Error ? e.message : "Setup failed")
    } finally {
      setLoading(false)
    }
  }

  const displayName = selectedProfile?.display_name ?? selectedProfile?.handle ?? selectedProfile?.did
  const avatarFallback = displayName?.charAt(0).toUpperCase() ?? "?"

  return (
    <Card>
      <CardHeader>
        <CardTitle>Attach Existing Account</CardTitle>
        <CardDescription>Enter the handle or DID of the account to attach. You'll verify ownership in the next step.</CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <div ref={containerRef}>
          <Label htmlFor="identifier">Account identifier</Label>
          <div className="relative mt-1.5">
            <Input
              id="identifier"
              placeholder="handle.bsky.social or did:plc:..."
              value={inputValue}
              onChange={(e) => handleInputChange(e.target.value)}
              onFocus={() => { if (suggestions.length > 0) setShowSuggestions(true) }}
              autoComplete="off"
              disabled={loading}
            />
            {resolving && (
              <span className="text-muted-foreground absolute right-3 top-1/2 -translate-y-1/2 text-xs">
                Resolving...
              </span>
            )}
            {showSuggestions && suggestions.length > 0 && (
              <div className="absolute z-50 mt-1 w-full rounded-md border bg-popover shadow-md">
                {suggestions.map((result, index) => {
                  const name = result.display_name ?? result.handle ?? result.did
                  const fallback = name.charAt(0).toUpperCase()
                  return (
                    <button
                      key={result.did}
                      type="button"
                      className={`flex w-full items-center gap-3 px-3 py-2 text-left text-sm transition-colors hover:bg-accent ${index === 0 ? "rounded-t-md" : ""} ${index === suggestions.length - 1 ? "rounded-b-md" : ""}`}
                      onMouseDown={(e) => {
                        e.preventDefault()
                        selectResult(result)
                      }}
                    >
                      <Avatar className="h-7 w-7 shrink-0">
                        {result.avatar && <AvatarImage src={result.avatar} alt="" />}
                        <AvatarFallback className="text-xs">{fallback}</AvatarFallback>
                      </Avatar>
                      <div className="min-w-0 flex-1">
                        {result.display_name && (
                          <p className="truncate font-medium">{result.display_name}</p>
                        )}
                        <p className="text-muted-foreground truncate text-xs">
                          {result.handle ? `@${result.handle}` : result.did}
                        </p>
                      </div>
                    </button>
                  )
                })}
              </div>
            )}
          </div>
        </div>

        {selectedProfile && (
          <div className="flex items-center gap-3 rounded-md border p-3">
            <Avatar className="h-10 w-10 shrink-0">
              {selectedProfile.avatar && <AvatarImage src={selectedProfile.avatar} alt="" />}
              <AvatarFallback>{avatarFallback}</AvatarFallback>
            </Avatar>
            <div className="min-w-0 flex-1">
              {selectedProfile.display_name && (
                <p className="truncate font-medium">{selectedProfile.display_name}</p>
              )}
              <p className="text-muted-foreground truncate text-sm">
                {selectedProfile.handle ? `@${selectedProfile.handle}` : selectedProfile.did}
              </p>
              <p className="text-muted-foreground truncate text-xs font-mono">{selectedProfile.did}</p>
            </div>
            <button
              type="button"
              onClick={clearSelection}
              className="text-muted-foreground hover:text-foreground text-xs shrink-0"
            >
              Change
            </button>
          </div>
        )}

        {error && <p className="text-destructive text-sm">{error}</p>}
        <div className="flex justify-end">
          <Button onClick={handleSubmit} disabled={loading || !inputValue.trim()}>
            {loading ? "Configuring..." : "Continue"}
          </Button>
        </div>
      </CardContent>
    </Card>
  )
}
