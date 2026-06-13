"use client"

import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card"
import { Button } from "@/components/ui/button"
import { CheckCircle2 } from "lucide-react"
import { useRouter } from "next/navigation"
import { completeSetup } from "@/lib/api"
import { useEffect } from "react"

interface SetupCompleteProps { identityMode: string | null }

export function SetupComplete({ identityMode }: SetupCompleteProps) {
  const router = useRouter()

  useEffect(() => {
    if (identityMode === "not_exposed") { completeSetup() }
  }, [identityMode])

  return (
    <Card>
      <CardHeader className="text-center">
        <div className="flex justify-center mb-4"><CheckCircle2 className="h-12 w-12 text-green-500" /></div>
        <CardTitle>Setup Complete</CardTitle>
        <CardDescription>
          {identityMode === "not_exposed"
            ? "Your HappyView instance is ready. You can configure service identity later from settings."
            : "Your HappyView instance is configured and ready to accept proxied requests."}
        </CardDescription>
      </CardHeader>
      <CardContent className="flex justify-center gap-3">
        <Button variant="outline" onClick={() => router.push("/dashboard/settings/service-identity")}>Service Identity Settings</Button>
        <Button onClick={() => router.push("/dashboard")}>Go to Dashboard</Button>
      </CardContent>
    </Card>
  )
}
