"use client"

import { useCallback, useEffect, useState } from "react"
import { getSetupStatus } from "@/lib/api"
import { SetupIdentityMode } from "./setup-identity-mode"
import { SetupConfigure } from "./setup-configure"
import { SetupAttachAuth } from "./setup-attach-auth"
import { SetupVerify } from "./setup-verify"
import { SetupComplete } from "./setup-complete"
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

  useEffect(() => {
    getSetupStatus()
      .then((status) => {
        if (status.setup_complete) {
          setCurrentStep("complete")
        } else if (status.plc_verified || (status.identity_mode === "not_exposed")) {
          setCurrentStep("complete")
        } else if (status.identity_configured) {
          setCurrentStep("verify")
        } else if (status.identity_mode) {
          setIdentityMode(status.identity_mode)
          setCurrentStep("configure")
        }
      })
      .finally(() => setLoading(false))
  }, [])

  const handleModeSelected = useCallback((mode: string) => {
    setIdentityMode(mode)
    if (mode === "not_exposed") {
      setCurrentStep("complete")
    } else {
      setCurrentStep("configure")
    }
  }, [])

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

  if (loading) {
    return <div className="flex items-center justify-center py-12">Loading...</div>
  }

  return (
    <Stepper value={currentStep} onValueChange={(v) => setCurrentStep(v as SetupStep)}>
      <StepperList>
        <StepperItem value="mode">
          <StepperTrigger><StepperIndicator /><StepperTitle>Identity Mode</StepperTitle></StepperTrigger>
          <StepperSeparator />
        </StepperItem>
        <StepperItem value="configure">
          <StepperTrigger><StepperIndicator /><StepperTitle>Configure</StepperTitle></StepperTrigger>
          <StepperSeparator />
        </StepperItem>
        {identityMode === "attach_account" && (
          <StepperItem value="attach-auth">
            <StepperTrigger><StepperIndicator /><StepperTitle>Authenticate</StepperTitle></StepperTrigger>
            <StepperSeparator />
          </StepperItem>
        )}
        <StepperItem value="verify">
          <StepperTrigger><StepperIndicator /><StepperTitle>Verify</StepperTitle></StepperTrigger>
          <StepperSeparator />
        </StepperItem>
        <StepperItem value="complete">
          <StepperTrigger><StepperIndicator /><StepperTitle>Complete</StepperTitle></StepperTrigger>
        </StepperItem>
      </StepperList>

      <div className="mt-8">
        {currentStep === "mode" && <SetupIdentityMode onComplete={handleModeSelected} />}
        {currentStep === "configure" && identityMode && <SetupConfigure mode={identityMode} onComplete={handleConfigureComplete} />}
        {currentStep === "attach-auth" && attachedDid && (
          <SetupAttachAuth
            attachedDid={attachedDid}
            attachedHandle={attachedHandle}
            onComplete={handleAttachAuthComplete}
          />
        )}
        {currentStep === "verify" && identityMode && <SetupVerify mode={identityMode} onComplete={handleVerifyComplete} />}
        {currentStep === "complete" && <SetupComplete identityMode={identityMode} />}
      </div>
    </Stepper>
  )
}
