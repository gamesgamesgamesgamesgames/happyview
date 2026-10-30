"use client";

import { createContext, useContext, useEffect, useRef, useState } from "react";
import { TriangleAlert } from "lucide-react";

import {
  InstanceConnecting,
  InstanceUnavailable,
} from "@/components/instance-unavailable";
import {
  type ConfigFailure,
  classifyConfigFailure,
  retryDelayMs,
} from "@/lib/config-load";

interface ConfigContextType {
  public_url: string;
  default_rate_limit_capacity: number;
  default_rate_limit_refill_rate: number;
  app_name: string | null;
  logo_url: string | null;
  platform_managed: boolean;
  configErrors: string[];
}

const ConfigContext = createContext<ConfigContextType>({
  public_url: "",
  default_rate_limit_capacity: 100,
  default_rate_limit_refill_rate: 2.0,
  app_name: null,
  logo_url: null,
  platform_managed: false,
  configErrors: [],
});

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
  );
}

/** Fetch `/config`, answering a failure as what it means for the dashboard. */
async function loadConfig(): Promise<
  { config: ConfigContextType } | { failure: ConfigFailure }
> {
  const host = window.location.host;
  let res: Response;
  try {
    res = await fetch(`${process.env.NEXT_PUBLIC_BASE_PATH || ""}/config`);
  } catch (e) {
    const detail = e instanceof Error ? e.message : String(e);
    return { failure: classifyConfigFailure(null, host, detail) };
  }
  if (!res.ok) {
    return {
      failure: classifyConfigFailure(
        res.status,
        host,
        `The server answered ${res.status}.`,
      ),
    };
  }
  const data = await res.json();
  return {
    config: {
      public_url: data.public_url,
      default_rate_limit_capacity: data.default_rate_limit_capacity,
      default_rate_limit_refill_rate: data.default_rate_limit_refill_rate,
      app_name: data.app_name ?? null,
      logo_url: data.logo_url ?? null,
      platform_managed: data.platform_managed === true,
      configErrors: Array.isArray(data.configErrors) ? data.configErrors : [],
    },
  };
}

export function ConfigProvider({ children }: { children: React.ReactNode }) {
  const [config, setConfig] = useState<ConfigContextType | null>(null);
  const [failure, setFailure] = useState<ConfigFailure | null>(null);
  const [retrying, setRetrying] = useState(false);
  const retryNow = useRef<() => void>(() => {});

  // An unreachable server is retried with backoff until it answers. A host
  // the server does not serve is not: asking again gets the same answer.
  useEffect(() => {
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let attempt = 0;
    const run = async () => {
      clearTimeout(timer);
      const outcome = await loadConfig();
      if (cancelled) return;
      setRetrying(false);
      if ("config" in outcome) {
        setFailure(null);
        setConfig(outcome.config);
        return;
      }
      setFailure(outcome.failure);
      if (outcome.failure.kind === "unreachable") {
        timer = setTimeout(run, retryDelayMs(attempt));
        attempt += 1;
      }
    };
    retryNow.current = run;
    run();
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, []);

  if (failure && !config) {
    return (
      <InstanceUnavailable
        failure={failure}
        retrying={retrying}
        onRetry={
          failure.kind === "unreachable"
            ? () => {
                setRetrying(true);
                retryNow.current();
              }
            : undefined
        }
      />
    );
  }

  if (!config) return <InstanceConnecting />;

  return (
    <ConfigContext.Provider value={config}>
      {config.configErrors.length > 0 && (
        <ConfigErrorBanner errors={config.configErrors} />
      )}
      {children}
    </ConfigContext.Provider>
  );
}

export function useConfig() {
  return useContext(ConfigContext);
}
