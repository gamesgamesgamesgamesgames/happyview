"use client"

import { useState } from "react"
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card"
import { Button } from "@/components/ui/button"
import { cn } from "@/lib/utils"

const MODES = [
  { value: "did_web", title: "Use did:web (domain-based)", description: "Auto-generate a DID document served from this domain. Simplest option — just needs DNS you already control." },
  { value: "attach_account", title: "Attach existing account", description: "Add a service entry to an existing AT Protocol account's DID document. Uses the PLC confirmation code flow." },
  { value: "did_plc", title: "Create did:plc", description: "Register a new DID in the PLC directory. Most durable — survives domain changes. HappyView manages the keypairs." },
  { value: "not_exposed", title: "Not exposed", description: "Skip identity setup. This instance won't support service proxying. You can configure this later in settings." },
]

interface SetupIdentityModeProps { onComplete: (mode: string) => void }

export function SetupIdentityMode({ onComplete }: SetupIdentityModeProps) {
  const [selected, setSelected] = useState<string | null>(null)

  return (
    <Card>
      <CardHeader>
        <CardTitle>How should this AppView be identified?</CardTitle>
        <CardDescription>Choose how other AT Protocol services discover and authenticate with this instance.</CardDescription>
      </CardHeader>
      <CardContent className="space-y-3">
        {MODES.map((mode) => (
          <button key={mode.value} type="button" className={cn(
            "w-full rounded-lg border-2 p-4 text-left transition-colors",
            selected === mode.value ? "border-primary bg-primary/5" : "border-border hover:border-primary/50"
          )} onClick={() => setSelected(mode.value)}>
            <div className="font-semibold">{mode.title}</div>
            <div className="text-muted-foreground text-sm mt-1">{mode.description}</div>
          </button>
        ))}
        <div className="flex justify-end pt-4">
          <Button disabled={!selected} onClick={() => selected && onComplete(selected)}>Continue</Button>
        </div>
      </CardContent>
    </Card>
  )
}
