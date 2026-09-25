// useSystemInfo: real system info for the Welcome step.

import { useState, useEffect } from "react";
import { api } from "../../../api/PondApiClient";
import type { SystemInfo } from "../onboarding.types";

interface SystemInfoState {
  health: { status: string; version?: string } | null;
  system: SystemInfo | null;
  loading: boolean;
}

export function useSystemInfo(): SystemInfoState {
  const [state, setState] = useState<SystemInfoState>({
    health: null,
    system: null,
    loading: true,
  });

  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const [health, system] = await Promise.allSettled([
          api.health(),
          api.getSystemInfo(),
        ]);
        if (cancelled) return;
        setState({
          health: health.status === "fulfilled" ? health.value : null,
          system: system.status === "fulfilled" ? system.value : null,
          loading: false,
        });
      } catch {
        if (!cancelled) setState((s) => ({ ...s, loading: false }));
      }
    })();
    return () => { cancelled = true; };
  }, []);

  return state;
}
