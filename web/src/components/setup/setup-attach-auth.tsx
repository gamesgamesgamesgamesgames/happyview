"use client"

import { useEffect, useState } from "react"
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card"
import { Button } from "@/components/ui/button"
import { confirmAttachAuth } from "@/lib/api"

const ATTACH_AUTH_STORAGE_KEY = "happyview_attach_auth"

interface AttachAuthPayload {
  attachedDid: string
  originalDid: string
}

interface SetupAttachAuthProps {
  attachedDid: string
  attachedHandle: string | null
  onComplete: () => void
}

export function SetupAttachAuth({ attachedDid, attachedHandle, onComplete }: SetupAttachAuthProps) {
  const [confirming, setConfirming] = useState(false)
  const [error, setError] = useState<string | null>(null)

  // On mount: check if we're returning from the OAuth redirect
  useEffect(() => {
    const stored = localStorage.getItem(ATTACH_AUTH_STORAGE_KEY)
    if (!stored) return

    let payload: AttachAuthPayload
    try {
      payload = JSON.parse(stored) as AttachAuthPayload
    } catch {
      localStorage.removeItem(ATTACH_AUTH_STORAGE_KEY)
      return
    }

    // Only act if the stored DID matches the current attached DID
    if (payload.attachedDid !== attachedDid) {
      localStorage.removeItem(ATTACH_AUTH_STORAGE_KEY)
      return
    }

    // We're returning from the OAuth flow — confirm and restore the admin session
    localStorage.removeItem(ATTACH_AUTH_STORAGE_KEY)
    setConfirming(true)

    confirmAttachAuth({ original_did: payload.originalDid })
      .then(() => onComplete())
      .catch((e) => {
        setError(e instanceof Error ? e.message : "Failed to restore admin session")
        setConfirming(false)
      })
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  function handleAuthenticate() {
    // We need the admin's current DID to restore it after OAuth.
    // Fetch it from /auth/me before redirecting.
    setConfirming(true)
    setError(null)

    fetch("/auth/me", { credentials: "same-origin" })
      .then((res) => {
        if (!res.ok) throw new Error("Failed to fetch current user")
        return res.json() as Promise<{ did: string }>
      })
      .then(({ did: originalDid }) => {
        const payload: AttachAuthPayload = { attachedDid, originalDid }
        localStorage.setItem(ATTACH_AUTH_STORAGE_KEY, JSON.stringify(payload))

        const handle = attachedHandle ?? attachedDid
        window.location.href = `/auth/login?handle=${encodeURIComponent(handle)}`
      })
      .catch((e) => {
        setError(e instanceof Error ? e.message : "Failed to start authentication")
        setConfirming(false)
      })
  }

  const displayName = attachedHandle ? `@${attachedHandle}` : attachedDid

  return (
    <Card>
      <CardHeader>
        <CardTitle>Authenticate Attached Account</CardTitle>
        <CardDescription>
          To authorize PLC changes, you need to authenticate as{" "}
          <span className="font-medium">{displayName}</span>.
          You'll be redirected to sign in, then returned here automatically.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="bg-muted rounded-lg p-4 text-sm space-y-1">
          <p className="font-medium">What happens next:</p>
          <ol className="list-decimal list-inside space-y-1 text-muted-foreground">
            <li>You'll be redirected to authenticate as {displayName}</li>
            <li>After sign-in you'll be returned to this page</li>
            <li>Your admin session will be restored automatically</li>
          </ol>
        </div>
        {error && <p className="text-destructive text-sm">{error}</p>}
        {confirming ? (
          <p className="text-muted-foreground text-sm">Restoring admin session...</p>
        ) : (
          <div className="flex justify-end">
            <Button onClick={handleAuthenticate}>
              Authenticate as {displayName}
            </Button>
          </div>
        )}
      </CardContent>
    </Card>
  )
}
