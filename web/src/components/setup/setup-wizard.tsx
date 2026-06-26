"use client"

import { useCallback, useEffect, useMemo, useRef, useState } from "react"
import { getSetupStatus, setSetupIdentity } from "@/lib/api"
import { SetupIdentityMode } from "./setup-identity-mode"
import { SetupConfigure } from "./setup-configure"
import { SetupAttachAuth } from "./setup-attach-auth"
import { SetupVerify } from "./setup-verify"
import { SetupComplete } from "./setup-complete"
import { Button } from "@/components/ui/button"
import { Skeleton } from "@/components/ui/skeleton"
import {
  Stepper, StepperItem, StepperList,
  StepperIndicator, StepperSeparator, StepperTitle, StepperTrigger,
} from "@/components/ui/stepper"

type SetupStep = "mode" | "configure" | "attach-auth" | "verify" | "complete"

export function SetupWizard() {
  const [currentStep, setCurrentStep] = useState<SetupStep>("mode")
  const [identityMode, setIdentityMode] = useState<string | null>(null)
  const [attachedDid, setAttachedDid] = useState<string | null>(null)
  const [attachedHandle, setAttachedHandle] = useState<string | null>(null)
  const [loading, setLoading] = useState(true)
  const [error, setError] = useState<string | null>(null)
  const lastFailedModeRef = useRef<string | null>(null)
  const stepContentRef = useRef<HTMLDivElement>(null)

  const initialLoadRef = useRef(true)
  useEffect(() => {
    if (initialLoadRef.current) {
      initialLoadRef.current = false
      return
    }
    stepContentRef.current?.focus()
  }, [currentStep])

  useEffect(() => {
    getSetupStatus()
      .then((status) => {
        if (status.setup_complete) {
          setCurrentStep("complete")
        } else if (status.plc_verified) {
          setCurrentStep("complete")
        } else if (status.identity_configured) {
          setIdentityMode(status.identity_mode)
          setCurrentStep("verify")
        } else if (status.identity_mode && status.identity_mode !== "not_exposed") {
          setIdentityMode(status.identity_mode)

          const pendingAuth = localStorage.getItem("happyview_attach_auth")
          if (status.identity_mode === "attach_account" && pendingAuth) {
            try {
              const payload = JSON.parse(pendingAuth) as { attachedDid: string }
              setAttachedDid(payload.attachedDid)
              setCurrentStep("attach-auth")
            } catch {
              setCurrentStep("configure")
            }
          } else {
            setCurrentStep("configure")
          }
        }
      })
      .catch(() => {})
      .finally(() => setLoading(false))
  }, [])

  const handleModeSelected = useCallback(async (mode: string) => {
    setIdentityMode(mode)
    setError(null)
    lastFailedModeRef.current = null
    if (mode === "not_exposed") {
      try {
        await setSetupIdentity({ mode: "not_exposed" })
        setCurrentStep("complete")
      } catch (e) {
        lastFailedModeRef.current = mode
        setError(e instanceof Error ? e.message : "Failed to save configuration. Check that your backend is running and try again.")
      }
    } else if (mode === "attach_account") {
      setCurrentStep("configure")
    } else {
      try {
        await setSetupIdentity({ mode })
        setCurrentStep("verify")
      } catch (e) {
        lastFailedModeRef.current = mode
        setError(e instanceof Error ? e.message : "Failed to configure identity. Check that your backend is running and try again.")
      }
    }
  }, [])

  const handleGoBack = useCallback(() => {
    setError(null)
    switch (currentStep) {
      case "configure":
        setCurrentStep("mode")
        break
      case "attach-auth":
        setCurrentStep("configure")
        break
      case "verify":
        setCurrentStep(identityMode === "attach_account" ? "configure" : "mode")
        break
    }
  }, [currentStep, identityMode])

  const handleConfigureComplete = useCallback((opts?: { attachedDid?: string; attachedHandle?: string | null }) => {
    if (opts?.attachedDid) {
      setAttachedDid(opts.attachedDid)
      setAttachedHandle(opts.attachedHandle ?? null)
      setCurrentStep("attach-auth")
    } else {
      setCurrentStep("verify")
    }
  }, [])

  const handleAttachAuthComplete = useCallback(() => {
    setCurrentStep("verify")
  }, [])

  const handleVerifyComplete = useCallback(() => {
    setCurrentStep("complete")
  }, [])

  const stepOrder = useMemo<SetupStep[]>(() => identityMode === "attach_account"
    ? ["mode", "configure", "attach-auth", "verify", "complete"]
    : ["mode", "verify", "complete"]
  , [identityMode])

  const handleStepperNav = useCallback((value: string) => {
    const target = value as SetupStep
    const currentIndex = stepOrder.indexOf(currentStep)
    const targetIndex = stepOrder.indexOf(target)
    if (targetIndex < 0 || targetIndex > currentIndex) return
    if (target === "mode") {
      setIdentityMode(null)
      setAttachedDid(null)
      setAttachedHandle(null)
      setError(null)
    }
    setCurrentStep(target)
  }, [currentStep, stepOrder])

  if (loading) {
    return (
      <div className="space-y-4" role="status" aria-label="Loading setup">
        <Skeleton className="h-10 w-full" />
        <Skeleton className="h-64 w-full rounded-xl" />
      </div>
    )
  }

  return (
    <Stepper value={currentStep} onValueChange={handleStepperNav}>
      <StepperList>
        <StepperItem value="mode">
          <StepperTrigger><StepperIndicator /><StepperTitle>Identity</StepperTitle></StepperTrigger>
          <StepperSeparator />
        </StepperItem>
        {identityMode === "attach_account" && (
          <>
            <StepperItem value="configure">
              <StepperTrigger><StepperIndicator /><StepperTitle>Account</StepperTitle></StepperTrigger>
              <StepperSeparator />
            </StepperItem>
            <StepperItem value="attach-auth">
              <StepperTrigger><StepperIndicator /><StepperTitle>Sign In</StepperTitle></StepperTrigger>
              <StepperSeparator />
            </StepperItem>
          </>
        )}
        <StepperItem value="verify">
          <StepperTrigger><StepperIndicator /><StepperTitle>{identityMode === "did_plc" ? "Key Backup" : identityMode === "attach_account" ? "Verify" : "Review"}</StepperTitle></StepperTrigger>
          <StepperSeparator />
        </StepperItem>
        <StepperItem value="complete">
          <StepperTrigger><StepperIndicator /><StepperTitle>Done</StepperTitle></StepperTrigger>
        </StepperItem>
      </StepperList>

      {error && (
        <div role="alert" className="mt-4 rounded-lg border border-destructive/20 bg-destructive/10 p-3 text-sm text-destructive flex items-center justify-between gap-3">
          <span>{error}</span>
          {lastFailedModeRef.current && (
            <Button variant="ghost" size="sm" className="text-destructive shrink-0" onClick={() => {
              if (lastFailedModeRef.current) handleModeSelected(lastFailedModeRef.current)
            }}>
              Try again
            </Button>
          )}
        </div>
      )}

      <div ref={stepContentRef} tabIndex={-1} className="mt-8 outline-none">
        {currentStep === "mode" && <SetupIdentityMode onComplete={handleModeSelected} />}
        {currentStep === "configure" && identityMode && (
          <SetupConfigure mode={identityMode} onComplete={handleConfigureComplete} onBack={handleGoBack} />
        )}
        {currentStep === "attach-auth" && attachedDid && (
          <SetupAttachAuth
            attachedDid={attachedDid}
            attachedHandle={attachedHandle}
            onComplete={handleAttachAuthComplete}
            onBack={handleGoBack}
          />
        )}
        {currentStep === "verify" && identityMode && (
          <SetupVerify mode={identityMode} onComplete={handleVerifyComplete} onBack={handleGoBack} />
        )}
        {currentStep === "complete" && <SetupComplete identityMode={identityMode} />}
      </div>
    </Stepper>
  )
}
