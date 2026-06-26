"use client"

import { useEffect, useState } from "react"
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { completeSetup, plcRequest, plcSubmit, plcRegister } from "@/lib/api"
import { docsUrl } from "@/lib/docs"
import { HelpTip } from "./help-tip"

interface SetupVerifyProps {
  mode: string
  onComplete: () => void
  onBack?: () => void
}

// ─── did:web ────────────────────────────────────────────────────────────────

function VerifyDidWeb({ onComplete, onBack }: { onComplete: () => void; onBack?: () => void }) {
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [didId, setDidId] = useState<string | null>(null)
  const [fetching, setFetching] = useState(true)
  const [fetchError, setFetchError] = useState(false)

  useEffect(() => {
    fetch("/.well-known/did.json")
      .then((res) => {
        if (!res.ok) throw new Error()
        return res.json()
      })
      .then((doc) => { if (doc?.id) setDidId(doc.id) })
      .catch(() => setFetchError(true))
      .finally(() => setFetching(false))
  }, [])

  const handleConfirm = async () => {
    setLoading(true)
    setError(null)
    try { await completeSetup(); onComplete() }
    catch (e) { setError(e instanceof Error ? e.message : "Failed to complete setup. Check your backend connection and try again.") }
    finally { setLoading(false) }
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>Review your domain identity</CardTitle>
        <CardDescription>A signing key has been generated and your identity document is ready. <a href={docsUrl("/getting-started/service-identity#domain-identity-didweb")} target="_blank" rel="noopener noreferrer" className="text-primary underline underline-offset-4 hover:text-primary/80">Learn more</a></CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        {fetching ? (
          <p className="text-muted-foreground text-sm" aria-live="polite">Checking identity document…</p>
        ) : (
          <dl className="space-y-3 text-sm">
            {fetchError && (
              <div role="alert" className="rounded-lg border border-destructive/20 bg-destructive/10 p-3 text-sm text-destructive">
                Could not load your DID document from <span className="font-mono">/.well-known/did.json</span>. Your identity may not be configured correctly.
              </div>
            )}
            {didId && (
              <div>
                <dt className="text-muted-foreground">Identity</dt>
                <dd className="font-mono mt-0.5">{didId}</dd>
              </div>
            )}
            <div>
              <dt className="text-muted-foreground">Document URL</dt>
              <dd className="font-mono mt-0.5">/.well-known/did.json</dd>
            </div>
            <div>
              <dt className="text-muted-foreground flex items-center gap-1.5">Signing key <HelpTip label="Used to authenticate data your AppView publishes to the AT Protocol network. Generated automatically and stored encrypted on your server." href="https://atproto.com/specs/cryptography" /></dt>
              <dd className="mt-0.5">P-256 keypair, encrypted at rest</dd>
            </div>
          </dl>
        )}
        <p className="text-muted-foreground text-sm">You can add service entries after setup from the Service Identity settings page.</p>
        {error && <p role="alert" className="text-destructive text-sm">{error}</p>}
        <div className="flex justify-between">
          {onBack ? (
            <Button variant="ghost" onClick={onBack}>Back</Button>
          ) : <div />}
          <Button onClick={handleConfirm} disabled={loading || fetching}>{loading ? "Completing…" : fetchError ? "Continue anyway" : "Looks good"}</Button>
        </div>
      </CardContent>
    </Card>
  )
}

// ─── attach_account ──────────────────────────────────────────────────────────

function VerifyAttachAccount({ onComplete, onBack }: { onComplete: () => void; onBack?: () => void }) {
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [token, setToken] = useState("")
  const [codeSent, setCodeSent] = useState(false)
  const [sendingCode, setSendingCode] = useState(false)

  useEffect(() => {
    let cancelled = false
    setSendingCode(true)
    setError(null)
    plcRequest()
      .then(() => { if (!cancelled) setCodeSent(true) })
      .catch((e) => { if (!cancelled) setError(e instanceof Error ? e.message : "Failed to send confirmation code. Check your connection and try again.") })
      .finally(() => { if (!cancelled) setSendingCode(false) })
    return () => { cancelled = true }
  }, [])

  const handleSendCode = async () => {
    setSendingCode(true)
    setError(null)
    try {
      await plcRequest()
      setCodeSent(true)
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to send confirmation code. Check your connection and try again.")
    } finally {
      setSendingCode(false)
    }
  }

  const handleSubmitToken = async () => {
    setLoading(true)
    setError(null)
    try {
      await plcSubmit(token)
      await completeSetup()
      onComplete()
    } catch (e) {
      setError(e instanceof Error ? e.message : "Verification failed. Check the code and try again.")
    } finally {
      setLoading(false)
    }
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>Enter your confirmation code</CardTitle>
        <CardDescription>
          {codeSent
            ? "A code has been sent to the email address on this account."
            : "We'll send a confirmation code to the email address on this account."}{" "}
          <a href={docsUrl("/getting-started/service-identity#linked-account")} target="_blank" rel="noopener noreferrer" className="text-primary underline underline-offset-4 hover:text-primary/80">Learn more</a>
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        {!codeSent ? (
          <Button onClick={handleSendCode} disabled={sendingCode}>
            {sendingCode ? "Sending…" : "Send Confirmation Code"}
          </Button>
        ) : (
          <form onSubmit={(e) => { e.preventDefault(); if (token && !loading) handleSubmitToken() }} className="space-y-4">
            <div>
              <Label htmlFor="token">Confirmation Code</Label>
              <Input id="token" placeholder="Paste confirmation code" value={token} onChange={(e) => setToken(e.target.value)} className="mt-1.5" aria-required="true" />
            </div>
            <Button variant="link" size="sm" className="px-0" type="button" onClick={handleSendCode} disabled={sendingCode}>
              {sendingCode ? "Sending…" : "Resend code"}
            </Button>
            {error && <p role="alert" className="text-destructive text-sm">{error}</p>}
            <div className="flex justify-between">
              {onBack ? (
                <Button variant="ghost" type="button" onClick={onBack}>Back</Button>
              ) : <div />}
              <Button type="submit" disabled={loading || !token}>
                {loading ? "Verifying…" : "Verify & Complete"}
              </Button>
            </div>
          </form>
        )}
        {!codeSent && error && <p role="alert" className="text-destructive text-sm">{error}</p>}
      </CardContent>
    </Card>
  )
}

// ─── did:plc ─────────────────────────────────────────────────────────────────

function VerifyDidPlc({ onComplete, onBack }: { onComplete: () => void; onBack?: () => void }) {
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [registering, setRegistering] = useState(true)
  const [registeredDid, setRegisteredDid] = useState<string | null>(null)
  const [regError, setRegError] = useState<string | null>(null)
  const [keyDownloaded, setKeyDownloaded] = useState(false)
  const [downloading, setDownloading] = useState(false)
  const [downloadError, setDownloadError] = useState<string | null>(null)

  useEffect(() => {
    plcRegister()
      .then((result) => {
        setRegisteredDid(result.did)
      })
      .catch((e) => {
        setRegError(e instanceof Error ? e.message : "Registration failed. Check your backend connection and try again.")
      })
      .finally(() => setRegistering(false))
  }, [])

  const handleDownloadKey = async () => {
    setDownloading(true)
    setDownloadError(null)
    try {
      const res = await fetch("/api/setup/rotation-key")
      if (!res.ok) throw new Error("Server returned an error")
      const blob = await res.blob()
      const url = URL.createObjectURL(blob)
      const a = document.createElement("a")
      a.href = url
      const disposition = res.headers.get("Content-Disposition")
      const filenameMatch = disposition?.match(/filename="?([^";\s]+)"?/)
      a.download = filenameMatch?.[1] ?? "rotation-key.json"
      document.body.appendChild(a)
      a.click()
      document.body.removeChild(a)
      URL.revokeObjectURL(url)
      setKeyDownloaded(true)
    } catch {
      setDownloadError("Failed to download the rotation key. Check your connection and try again.")
    } finally {
      setDownloading(false)
    }
  }

  const handleConfirm = async () => {
    setLoading(true)
    setError(null)
    try { await completeSetup(); onComplete() }
    catch (e) { setError(e instanceof Error ? e.message : "Failed to complete setup. Check your backend connection and try again.") }
    finally { setLoading(false) }
  }

  if (registering) {
    return (
      <Card>
        <CardHeader><CardTitle>Registering your identity…</CardTitle></CardHeader>
        <CardContent>
          <p className="text-muted-foreground text-sm" aria-live="polite">Creating your identity in the AT Protocol directory. This usually takes a few seconds.</p>
        </CardContent>
      </Card>
    )
  }

  if (regError) {
    return (
      <Card>
        <CardHeader><CardTitle>Registration failed</CardTitle></CardHeader>
        <CardContent className="space-y-4">
          <p role="alert" className="text-destructive text-sm">{regError}</p>
          {onBack && (
            <Button variant="ghost" onClick={onBack}>Back</Button>
          )}
        </CardContent>
      </Card>
    )
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>Save your rotation key</CardTitle>
        <CardDescription>Your identity is registered. Download the rotation key now — you won&apos;t be able to access it again after this step. <a href={docsUrl("/getting-started/service-identity#network-identity-didplc")} target="_blank" rel="noopener noreferrer" className="text-primary underline underline-offset-4 hover:text-primary/80">Learn more</a></CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <dl className="space-y-3 text-sm">
          {registeredDid && (
            <div>
              <dt className="text-muted-foreground">Identity</dt>
              <dd className="font-mono mt-0.5">{registeredDid}</dd>
            </div>
          )}
          <div>
            <dt className="text-muted-foreground flex items-center gap-1.5">Signing key <HelpTip label="Used to authenticate data your AppView publishes to the AT Protocol network. Generated automatically and stored encrypted on your server." href="https://atproto.com/specs/cryptography" /></dt>
            <dd className="mt-0.5">P-256 keypair, encrypted at rest</dd>
          </div>
          <div>
            <dt className="text-muted-foreground flex items-center gap-1.5">Rotation key <HelpTip label="Controls your did:plc identity. If your server goes down, this key is the only way to recover or update your identity. Store it somewhere safe — you won't be able to access it again after setup." href="https://atproto.com/specs/did" /></dt>
            <dd className="mt-0.5">Generated separately — download it below</dd>
          </div>
        </dl>
        <div id="rotation-key-warning" className="bg-destructive/10 border border-destructive/20 rounded-lg p-4">
          <p className="text-sm font-medium text-destructive">If you lose this key and this HappyView instance goes down, you won&apos;t be able to recover or update your identity.</p>
        </div>
        <Button variant="outline" onClick={handleDownloadKey} disabled={downloading} aria-describedby="rotation-key-warning">
          {downloading ? "Downloading…" : keyDownloaded ? "Download Again" : "Download Rotation Key"}
        </Button>
        {downloadError && <p role="alert" className="text-destructive text-sm">{downloadError}</p>}
        {!keyDownloaded ? (
          <p className="text-muted-foreground text-xs">You must download the rotation key before continuing.</p>
        ) : (
          <p className="text-muted-foreground text-xs">Store this file somewhere safe and offline — a password manager, encrypted USB drive, or secure backup.</p>
        )}
        {error && <p role="alert" className="text-destructive text-sm">{error}</p>}
        <div className="flex justify-between">
          {onBack ? (
            <Button variant="ghost" onClick={onBack}>Back</Button>
          ) : <div />}
          <Button onClick={handleConfirm} disabled={loading || !keyDownloaded}>
            {loading ? "Completing…" : "Continue"}
          </Button>
        </div>
      </CardContent>
    </Card>
  )
}

// ─── Root dispatcher ─────────────────────────────────────────────────────────

export function SetupVerify({ mode, onComplete, onBack }: SetupVerifyProps) {
  if (mode === "did_web") return <VerifyDidWeb onComplete={onComplete} onBack={onBack} />
  if (mode === "attach_account") return <VerifyAttachAccount onComplete={onComplete} onBack={onBack} />
  if (mode === "did_plc") return <VerifyDidPlc onComplete={onComplete} onBack={onBack} />
  return null
}
