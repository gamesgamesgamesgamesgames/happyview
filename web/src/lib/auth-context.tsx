"use client";

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useState,
} from "react";

interface AuthContextType {
  did: string | null;
  login: (handle: string) => Promise<void>;
  logout: () => Promise<void>;
  loading: boolean;
  error: string | null;
}

const AuthContext = createContext<AuthContextType>({
  did: null,
  login: async () => {},
  logout: async () => {},
  loading: true,
  error: null,
});

export function AuthProvider({ children }: { children: React.ReactNode }) {
  const [did, setDid] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    // ATProto loopback client IDs require 127.0.0.1, not localhost.
    if (window.location.hostname === "localhost") {
      window.location.hostname = "127.0.0.1";
      return;
    }

    let cancelled = false;

    async function init() {
      try {
        // Check if the user has a valid session cookie
        const resp = await fetch(
          `${process.env.NEXT_PUBLIC_BASE_PATH || ""}/auth/me`,
          { credentials: "same-origin" },
        );
        if (resp.ok) {
          const data = await resp.json();
          if (!cancelled && data.did) {
            setDid(data.did);
          }
        }
      } catch (e) {
        if (!cancelled) {
          console.error("Auth init error:", e);
          setError(e instanceof Error ? e.message : String(e));
        }
      } finally {
        if (!cancelled) setLoading(false);
      }
    }

    init();
    return () => {
      cancelled = true;
    };
  }, []);

  const login = useCallback(async (handle: string) => {
    setError(null);

    const resp = await fetch(
      `${process.env.NEXT_PUBLIC_BASE_PATH || ""}/auth/login?handle=${encodeURIComponent(handle)}`,
      {
        credentials: "same-origin",
      },
    );

    if (!resp.ok) {
      const text = await resp.text();
      throw new Error(`Login failed: ${text}`);
    }

    const data = await resp.json();
    // Redirect to the authorization URL
    window.location.href = data.url;
  }, []);

  const logout = useCallback(async () => {
    try {
      await fetch(`${process.env.NEXT_PUBLIC_BASE_PATH || ""}/auth/logout`, {
        method: "POST",
        credentials: "same-origin",
      });
    } catch {
      // Best-effort revocation
    }
    setDid(null);
  }, []);

  if (loading) return null;

  return (
    <AuthContext.Provider
      value={{
        did,
        login,
        logout,
        loading,
        error,
      }}
    >
      {children}
    </AuthContext.Provider>
  );
}

export function useAuth() {
  return useContext(AuthContext);
}
