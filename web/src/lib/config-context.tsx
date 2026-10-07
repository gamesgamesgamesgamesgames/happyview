"use client"

import { createContext, useCallback, useContext, useEffect, useState } from "react"
import { TriangleAlert } from "lucide-react"

interface Features {
  spaces: boolean
  space_inspector: boolean
}

interface Config {
  public_url: string
  default_rate_limit_capacity: number
  default_rate_limit_refill_rate: number
  app_name: string | null
  logo_url: string | null
  features: Features
  platform_managed: boolean
  configErrors: string[]
}

interface ConfigContextType extends Config {
  /** Re-fetch `/config`, e.g. after a setting that changes a feature flag. */
  refreshConfig: () => Promise<void>
}

const ConfigContext = createContext<ConfigContextType>({
  public_url: "",
  default_rate_limit_capacity: 100,
  default_rate_limit_refill_rate: 2.0,
  app_name: null,
  logo_url: null,
  features: { spaces: false, space_inspector: false },
  platform_managed: false,
  configErrors: [],
  refreshConfig: async () => {},
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
  const [config, setConfig] = useState<Config | null>(null)
  const [error, setError] = useState<string | null>(null)

  const refreshConfig = useCallback(async () => {
    try {
      const res = await fetch(`${process.env.NEXT_PUBLIC_BASE_PATH || ""}/config`)
      if (!res.ok) throw new Error(`Config fetch failed: ${res.status}`)
      const data = await res.json()
      setConfig({
        public_url: data.public_url,
        default_rate_limit_capacity: data.default_rate_limit_capacity,
        default_rate_limit_refill_rate: data.default_rate_limit_refill_rate,
        app_name: data.app_name ?? null,
        logo_url: data.logo_url ?? null,
        features: {
          spaces: data.features?.spaces === true,
          space_inspector: data.features?.space_inspector === true,
        },
        platform_managed: data.platform_managed === true,
        configErrors: Array.isArray(data.configErrors) ? data.configErrors : [],
      })
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e))
    }
  }, [])

  useEffect(() => {
    refreshConfig()
  }, [refreshConfig])

  if (error) {
    return <div style={{ padding: "2rem", color: "red" }}>Failed to load config: {error}</div>
  }

  if (!config) return null

  return (
    <ConfigContext.Provider value={{ ...config, refreshConfig }}>
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
