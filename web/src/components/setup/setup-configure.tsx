"use client"

import { useCallback, useEffect, useId, useRef, useState } from "react"
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { Avatar, AvatarFallback, AvatarImage } from "@/components/ui/avatar"
import { setSetupIdentity, resolveIdentity, type ResolveResult } from "@/lib/api"

interface SetupConfigureProps {
  mode: string
  onComplete: (opts?: { attachedDid?: string; attachedHandle?: string | null }) => void
  onBack?: () => void
}

export function SetupConfigure({ mode, onComplete, onBack }: SetupConfigureProps) {
  if (mode === "attach_account") {
    return <AttachAccountForm onComplete={(opts) => onComplete(opts)} onBack={onBack} />
  }

  return null
}

function AttachAccountForm({ onComplete, onBack }: {
  onComplete: (opts: { attachedDid: string; attachedHandle: string | null }) => void
  onBack?: () => void
}) {
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [inputValue, setInputValue] = useState("")
  const [suggestions, setSuggestions] = useState<ResolveResult[]>([])
  const [showSuggestions, setShowSuggestions] = useState(false)
  const [selectedProfile, setSelectedProfile] = useState<ResolveResult | null>(null)
  const [resolving, setResolving] = useState(false)
  const [focusedIndex, setFocusedIndex] = useState(-1)
  const debounceRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  const containerRef = useRef<HTMLDivElement>(null)
  const listboxId = useId()

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
      setShowSuggestions(true)
      setFocusedIndex(-1)
    } catch {
      setSuggestions([])
      setShowSuggestions(false)
      setFocusedIndex(-1)
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
      setError(e instanceof Error ? e.message : "Failed to link account. Check the identifier and try again.")
    } finally {
      setLoading(false)
    }
  }

  const trimmedInput = inputValue.trim()
  const looksValid = selectedProfile != null || /^did:[a-z]+:.+/.test(trimmedInput) || trimmedInput.includes(".")
  const showFormatHint = trimmedInput.length >= 2 && !looksValid && !resolving

  const displayName = selectedProfile?.display_name ?? selectedProfile?.handle ?? selectedProfile?.did
  const avatarFallback = displayName?.charAt(0).toUpperCase() ?? "?"
  const hasSuggestions = showSuggestions && suggestions.length > 0
  const showEmpty = showSuggestions && suggestions.length === 0 && !resolving && trimmedInput.length >= 2

  return (
    <Card>
      <CardHeader>
        <CardTitle>Find your account</CardTitle>
        <CardDescription>Search for the AT Protocol account you want to link to this AppView.</CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <div ref={containerRef}>
          <Label htmlFor="identifier">Handle or DID</Label>
          <div className="relative mt-1.5">
            <Input
              id="identifier"
              placeholder="e.g. alice.bsky.social"
              value={inputValue}
              onChange={(e) => handleInputChange(e.target.value)}
              onFocus={() => { if (suggestions.length > 0) setShowSuggestions(true) }}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  if (hasSuggestions && focusedIndex >= 0) {
                    e.preventDefault()
                    selectResult(suggestions[focusedIndex])
                  } else if (!hasSuggestions && looksValid && trimmedInput && !loading) {
                    e.preventDefault()
                    handleSubmit()
                  }
                  return
                }
                if (!hasSuggestions) return
                if (e.key === "ArrowDown") {
                  e.preventDefault()
                  setFocusedIndex((i) => (i + 1) % suggestions.length)
                } else if (e.key === "ArrowUp") {
                  e.preventDefault()
                  setFocusedIndex((i) => (i <= 0 ? suggestions.length - 1 : i - 1))
                } else if (e.key === "Escape") {
                  setShowSuggestions(false)
                  setFocusedIndex(-1)
                }
              }}
              autoComplete="off"
              disabled={loading}
              aria-required="true"
              role="combobox"
              aria-expanded={hasSuggestions}
              aria-controls={listboxId}
              aria-autocomplete="list"
              aria-activedescendant={focusedIndex >= 0 ? `${listboxId}-option-${focusedIndex}` : undefined}
            />
            {resolving && (
              <span className="text-muted-foreground absolute right-3 top-1/2 -translate-y-1/2 text-xs" aria-live="polite">
                Resolving…
              </span>
            )}
            {hasSuggestions && (
              <div id={listboxId} role="listbox" aria-label="Search results" className="absolute z-50 mt-1 w-full rounded-md border bg-popover shadow-md">
                {suggestions.map((result, index) => {
                  const name = result.display_name ?? result.handle ?? result.did
                  const fallback = name.charAt(0).toUpperCase()
                  return (
                    <button
                      key={result.did}
                      id={`${listboxId}-option-${index}`}
                      type="button"
                      role="option"
                      aria-selected={focusedIndex === index}
                      className={`flex w-full items-center gap-3 px-3 py-2 text-left text-sm transition-colors hover:bg-accent ${focusedIndex === index ? "bg-accent" : ""} ${index === 0 ? "rounded-t-md" : ""} ${index === suggestions.length - 1 ? "rounded-b-md" : ""}`}
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
            {showEmpty && (
              <div className="absolute z-50 mt-1 w-full rounded-md border bg-popover p-3 shadow-md">
                <p className="text-muted-foreground text-sm">No accounts found. Try a full handle (e.g. alice.bsky.social) or a DID.</p>
              </div>
            )}
          </div>
          {showFormatHint && (
            <p className="text-muted-foreground text-xs mt-1.5">Enter a handle (e.g. alice.bsky.social) or a DID (e.g. did:plc:...).</p>
          )}
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
            <Button
              variant="ghost"
              size="sm"
              onClick={clearSelection}
              aria-label="Clear selection and search again"
            >
              Change
            </Button>
          </div>
        )}

        {error && <p role="alert" className="text-destructive text-sm">{error}</p>}
        <div className="flex justify-between">
          {onBack ? (
            <Button variant="ghost" onClick={onBack}>Back</Button>
          ) : <div />}
          <Button onClick={handleSubmit} disabled={loading || !trimmedInput || !looksValid}>
            {loading ? "Configuring…" : "Continue"}
          </Button>
        </div>
      </CardContent>
    </Card>
  )
}
