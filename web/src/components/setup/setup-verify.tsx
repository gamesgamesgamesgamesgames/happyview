"use client"

import { useEffect, useState } from "react"
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card"
import { Button } from "@/components/ui/button"
import { Input } from "@/components/ui/input"
import { Label } from "@/components/ui/label"
import { completeSetup, plcRequest, plcSubmit, plcRegister } from "@/lib/api"

interface SetupVerifyProps { mode: string; onComplete: () => void }

// ─── did:web ────────────────────────────────────────────────────────────────

function VerifyDidWeb({ onComplete }: { onComplete: () => void }) {
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [didDoc, setDidDoc] = useState<Record<string, unknown> | null>(null)
  const [fetchingDidDoc, setFetchingDidDoc] = useState(false)
  const [fetchError, setFetchError] = useState<string | null>(null)

  useEffect(() => {
    const fetchDidDoc = async () => {
      setFetchingDidDoc(true)
      setFetchError(null)
      try {
        const response = await fetch("/.well-known/did.json")
        if (!response.ok) {
          if (response.status === 404) {
            setFetchError("DID document not yet available. Add service entries to generate it.")
          } else {
            setFetchError(`Failed to fetch DID document (${response.status})`)
          }
          return
        }
        const doc = await response.json()
        setDidDoc(doc)
      } catch (e) {
        setFetchError(e instanceof Error ? e.message : "Failed to fetch DID document")
      } finally {
        setFetchingDidDoc(false)
      }
    }

    fetchDidDoc()
  }, [])

  const handleConfirm = async () => {
    setLoading(true)
    setError(null)
    try { await completeSetup(); onComplete() }
    catch (e) { setError(e instanceof Error ? e.message : "Verification failed") }
    finally { setLoading(false) }
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>Verify DID Document</CardTitle>
        <CardDescription>Review the DID document that will be served at <code className="bg-muted px-1.5 py-0.5 rounded text-sm">/.well-known/did.json</code>.</CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        {fetchingDidDoc && <p className="text-muted-foreground text-sm">Loading DID document...</p>}
        {fetchError && <p className="text-muted-foreground text-sm">{fetchError}</p>}
        {didDoc && (
          <pre className="bg-slate-950 text-slate-50 p-4 rounded-lg overflow-x-auto font-mono text-xs border border-slate-800">
            {JSON.stringify(didDoc, null, 2)}
          </pre>
        )}
        {!fetchingDidDoc && !didDoc && !fetchError && (
          <p className="text-muted-foreground text-sm">The DID document will be generated from your service entries. You can add service entries after setup.</p>
        )}
        {error && <p className="text-destructive text-sm">{error}</p>}
        <div className="flex justify-end">
          <Button onClick={handleConfirm} disabled={loading}>{loading ? "Completing..." : "Looks Good"}</Button>
        </div>
      </CardContent>
    </Card>
  )
}

// ─── attach_account ──────────────────────────────────────────────────────────

function VerifyAttachAccount({ onComplete }: { onComplete: () => void }) {
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [token, setToken] = useState("")
  const [codeSent, setCodeSent] = useState(false)
  const [sendingCode, setSendingCode] = useState(false)

  useEffect(() => {
    handleSendCode()
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  const handleSendCode = async () => {
    setSendingCode(true)
    setError(null)
    try {
      await plcRequest()
      setCodeSent(true)
    } catch (e) {
      setError(e instanceof Error ? e.message : "Failed to send code")
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
      setError(e instanceof Error ? e.message : "Verification failed")
    } finally {
      setLoading(false)
    }
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>Verify Account Ownership</CardTitle>
        <CardDescription>
          {codeSent
            ? "A confirmation code has been sent to the account's email. Enter it below."
            : "We'll send a confirmation code to the account's email to verify ownership."}
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        {!codeSent ? (
          <Button onClick={handleSendCode} disabled={sendingCode}>
            {sendingCode ? "Sending..." : "Send Confirmation Code"}
          </Button>
        ) : (
          <>
            <div>
              <Label htmlFor="token">Confirmation Code</Label>
              <Input id="token" placeholder="Paste confirmation code" value={token} onChange={(e) => setToken(e.target.value)} className="mt-1.5" />
            </div>
            <button type="button" className="text-sm text-primary underline" onClick={handleSendCode} disabled={sendingCode}>
              {sendingCode ? "Sending..." : "Resend code"}
            </button>
          </>
        )}
        {error && <p className="text-destructive text-sm">{error}</p>}
        {codeSent && (
          <div className="flex justify-end">
            <Button onClick={handleSubmitToken} disabled={loading || !token}>
              {loading ? "Verifying..." : "Verify & Complete"}
            </Button>
          </div>
        )}
      </CardContent>
    </Card>
  )
}

// ─── did:plc ─────────────────────────────────────────────────────────────────

function VerifyDidPlc({ onComplete }: { onComplete: () => void }) {
  const [loading, setLoading] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [registering, setRegistering] = useState(true)
  const [registeredDid, setRegisteredDid] = useState<string | null>(null)
  const [regError, setRegError] = useState<string | null>(null)

  useEffect(() => {
    plcRegister()
      .then((result) => {
        setRegisteredDid(result.did)
      })
      .catch((e) => {
        setRegError(e instanceof Error ? e.message : "Registration failed")
      })
      .finally(() => setRegistering(false))
  }, [])

  const handleConfirm = async () => {
    setLoading(true)
    setError(null)
    try { await completeSetup(); onComplete() }
    catch (e) { setError(e instanceof Error ? e.message : "Verification failed") }
    finally { setLoading(false) }
  }

  if (registering) {
    return (
      <Card>
        <CardHeader><CardTitle>Registering DID...</CardTitle></CardHeader>
        <CardContent>
          <p className="text-muted-foreground text-sm">Creating your DID in the PLC directory...</p>
        </CardContent>
      </Card>
    )
  }

  if (regError) {
    return (
      <Card>
        <CardHeader><CardTitle>Registration Failed</CardTitle></CardHeader>
        <CardContent>
          <p className="text-destructive text-sm">{regError}</p>
        </CardContent>
      </Card>
    )
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>Export Rotation Key</CardTitle>
        <CardDescription>Your DID has been registered. Export the rotation key now — this is your only chance to back it up.</CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        {registeredDid && (
          <div className="bg-green-50 dark:bg-green-950/30 border border-green-200 dark:border-green-800 rounded-lg p-3">
            <div className="text-xs font-medium text-green-700 dark:text-green-400">Your DID</div>
            <code className="text-sm font-mono">{registeredDid}</code>
          </div>
        )}
        <div className="bg-destructive/10 border border-destructive/20 rounded-lg p-4">
          <p className="text-sm font-medium text-destructive">Losing the rotation key means losing the ability to update this DID document if this HappyView instance is lost.</p>
        </div>
        <div className="flex gap-2">
          <Button variant="outline" onClick={() => { window.location.href = "/api/setup/rotation-key" }}>Download Rotation Key</Button>
        </div>
        {error && <p className="text-destructive text-sm">{error}</p>}
        <div className="flex justify-end">
          <Button onClick={handleConfirm} disabled={loading}>{loading ? "Completing..." : "Continue"}</Button>
        </div>
      </CardContent>
    </Card>
  )
}

// ─── Root dispatcher ─────────────────────────────────────────────────────────

export function SetupVerify({ mode, onComplete }: SetupVerifyProps) {
  if (mode === "did_web") return <VerifyDidWeb onComplete={onComplete} />
  if (mode === "attach_account") return <VerifyAttachAccount onComplete={onComplete} />
  if (mode === "did_plc") return <VerifyDidPlc onComplete={onComplete} />
  return null
}
