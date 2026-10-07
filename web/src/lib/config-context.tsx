"use client"

import { createContext, useContext, useEffect, useState } from "react"
import { TriangleAlert } from "lucide-react"

interface ConfigContextType {
  public_url: string
  default_rate_limit_capacity: number
  default_rate_limit_refill_rate: number
  app_name: string | null
  logo_url: string | null
  platform_managed: boolean
  configErrors: string[]
}

const ConfigContext = createContext<ConfigContextType>({
  public_url: "",
  default_rate_limit_capacity: 100,
  default_rate_limit_refill_rate: 2.0,
  app_name: null,
  logo_url: null,
  platform_managed: false,
  configErrors: [],
})

function ConfigErrorBanner({ errors }: { errors: string[] }) {
  return (
    <div
      role="alert"
      className="border-b border-destructive/30 bg-destructive/10 text-destructive px-4 py-3"
    >
      <div className="mx-auto flex max-w-5xl items-start gap-3">
        <TriangleAlert className="mt-0.5 size-5 shrink-0" aria-hidden="true" />
        <div className="space-y-1 text-sm">
          <p className="font-semibold">Server misconfiguration</p>
          {errors.map((err, i) => (
            <p key={i} className="text-destructive/90">
              {err}
            </p>
          ))}
          <p className="text-destructive/80">
            The server is running, but the affected functionality stays disabled
            until this is fixed and the server is restarted.
          </p>
        </div>
      </div>
    </div>
  )
}

export function ConfigProvider({ children }: { children: React.ReactNode }) {
  const [config, setConfig] = useState<ConfigContextType | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    fetch(`${process.env.NEXT_PUBLIC_BASE_PATH || ""}/config`)
      .then((res) => {
        if (!res.ok) throw new Error(`Config fetch failed: ${res.status}`)
        return res.json()
      })
      .then((data) => {
        setConfig({
          public_url: data.public_url,
          default_rate_limit_capacity: data.default_rate_limit_capacity,
          default_rate_limit_refill_rate: data.default_rate_limit_refill_rate,
          app_name: data.app_name ?? null,
          logo_url: data.logo_url ?? null,
          platform_managed: data.platform_managed === true,
          configErrors: Array.isArray(data.configErrors) ? data.configErrors : [],
        })
      })
      .catch((e) => setError(e.message))
  }, [])

  if (error) {
    return <div style={{ padding: "2rem", color: "red" }}>Failed to load config: {error}</div>
  }

  if (!config) return null

  return (
    <ConfigContext.Provider value={config}>
      {config.configErrors.length > 0 && (
        <ConfigErrorBanner errors={config.configErrors} />
      )}
      {children}
    </ConfigContext.Provider>
  )
}

export function useConfig() {
  return useContext(ConfigContext)
}
