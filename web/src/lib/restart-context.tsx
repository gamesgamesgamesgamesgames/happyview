"use client";

import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState } from "react";

interface RestartContextType {
  reasons: string[];
  addReason: (key: string, reason: string) => void;
  removeReason: (key: string) => void;
}

const RestartContext = createContext<RestartContextType>({
  reasons: [],
  addReason: () => {},
  removeReason: () => {},
});

export function RestartProvider({ children }: { children: React.ReactNode }) {
  const [reasonMap, setReasonMap] = useState<Record<string, string>>({});
  const reasonMapRef = useRef(reasonMap);
  useEffect(() => {
    reasonMapRef.current = reasonMap;
  }, [reasonMap]);

  const addReason = useCallback((key: string, reason: string) => {
    if (reasonMapRef.current[key] === reason) return;
    setReasonMap((prev) => ({ ...prev, [key]: reason }));
  }, []);

  const removeReason = useCallback((key: string) => {
    setReasonMap((prev) => {
      if (!(key in prev)) return prev;
      const next = { ...prev };
      delete next[key];
      return next;
    });
  }, []);

  const reasons = useMemo(() => Object.values(reasonMap), [reasonMap]);

  return (
    <RestartContext.Provider value={{ reasons, addReason, removeReason }}>
      {children}
    </RestartContext.Provider>
  );
}

export function useRestart() {
  return useContext(RestartContext);
}
