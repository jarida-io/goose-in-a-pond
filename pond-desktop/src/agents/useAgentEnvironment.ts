import { useCallback, useEffect, useState } from "react";
import { api } from "../api/PondApiClient";
import type { Device, Extension, Settings } from "../api/types";
import type { MemberName } from "./agentWords";
import { useAgentPrefills, type PrefillsState } from "./useAgentPrefills";

/** What must be on before an agent starts by itself; each is a setting the pond checks per tick. */
export interface ActivationGate {
  delegation: boolean;
  activations: boolean;
  longTasks: boolean;
}

/** `null` means not known yet, which is not a reason to warn anyone. */
export function gateOpen(gate: ActivationGate | null): boolean {
  return gate === null || (gate.delegation && gate.activations && gate.longTasks);
}

/** What the builder and the cards know about the pond. Each part is `null` until it has loaded, or if it could not. */
export interface AgentEnvironment {
  extensions: Extension[] | null;
  members: MemberName[] | null;
  devices: Device[] | null;
  gate: ActivationGate | null;
  timezone: string | null;
  enableDelegation: () => Promise<void>;
  prefills: PrefillsState;
}

/** The two switches the desktop has no field for are read off the settings body, never written. */
type GateSettings = Settings & { agent_activations_enabled?: boolean; long_tasks_enabled?: boolean };

export function useAgentEnvironment(): AgentEnvironment {
  const [extensions, setExtensions] = useState<Extension[] | null>(null);
  const [members, setMembers] = useState<MemberName[] | null>(null);
  const [devices, setDevices] = useState<Device[] | null>(null);
  const [gate, setGate] = useState<ActivationGate | null>(null);
  const [timezone, setTimezone] = useState<string | null>(null);

  useEffect(() => {
    let live = true;
    api
      .listExtensions()
      .then((r) => {
        if (live) setExtensions(r.extensions ?? []);
      })
      .catch(() => {});
    api
      .listProfiles()
      .then((r) => {
        if (live) setMembers((r.profiles ?? []).map((p) => ({ id: p.id, name: p.display_name })));
      })
      .catch(() => {});
    api
      .listDevices()
      .then((d) => {
        if (live) setDevices(d);
      })
      .catch(() => {});
    api
      .getSettings()
      .then((s) => {
        if (!live) return;
        const g = s as GateSettings;
        setGate({
          delegation: g.ext_orchestrator_enabled === true,
          activations: g.agent_activations_enabled !== false,
          longTasks: g.long_tasks_enabled !== false,
        });
        setTimezone(g.timezone ?? null);
      })
      .catch(() => {});
    return () => {
      live = false;
    };
  }, []);

  const enableDelegation = useCallback(async () => {
    await api.updateSettings({ ext_orchestrator_enabled: true });
    setGate((g) => (g ? { ...g, delegation: true } : g));
  }, []);

  const prefills = useAgentPrefills();

  return { extensions, members, devices, gate, timezone, enableDelegation, prefills };
}
