// Everything a Models screen needs to run the Memory job: which model holds it, what the pond says
// about the embedder, and the three things a person can do (use a model, turn it on, try again).

import { useCallback, useMemo } from "react";
import { api } from "../api/PondApiClient";
import type { ModelEntry } from "../api/types";
import {
  embedderLine, memoryHolder, memoryIsOff, memoryModels, type EmbedderLine,
} from "../sections/models/memoryEmbedder";
import type { ModelActions } from "./useModelActions";
import type { UseModels } from "./useModels";
import { useEmbedderHealth } from "./useEmbedderHealth";
import { useOffline } from "./useOffline";

export interface MemoryJob {
  rows: ModelEntry[];
  holder: ReturnType<typeof memoryHolder>;
  off: boolean;
  /** The pond reaches nothing outside the house, so a file that is not here cannot be fetched. */
  offline: boolean;
  line: EmbedderLine | null;
  retrying: boolean;
  useModel: (m: ModelEntry) => Promise<void>;
  turnOn: () => Promise<void>;
  retry: () => Promise<void>;
}

export function useMemoryJob(
  data: Pick<UseModels, "models" | "roles" | "transferFor" | "reloadDownloads" | "reloadRoles">,
  actions: Pick<ModelActions, "useFor" | "setBusy">,
  say: (text: string, ok?: boolean) => void,
): MemoryJob {
  const { models, roles, transferFor, reloadDownloads, reloadRoles } = data;
  // The pond fetches a chosen file only once it has a quiet moment, so the downloads are watched too.
  const embedder = useEmbedderHealth({ onWatch: () => void reloadDownloads() });
  const { refresh, retry: askAgain } = embedder;
  const offline = useOffline();

  const rows = useMemo(() => memoryModels(models), [models]);
  const holder = useMemo(() => memoryHolder(models, roles), [models, roles]);
  const downloading = embedder.downloading || (holder?.entry ? transferFor(holder.entry) !== null : false);
  const line = embedderLine(embedder.health, { downloading });

  const useModel = useCallback(
    async (m: ModelEntry) => {
      await actions.useFor(m, "embedding");
      void refresh();
    },
    [actions, refresh],
  );

  const turnOn = useCallback(async () => {
    actions.setBusy(true);
    try {
      await api.updateSettings({ embedding_provider: "auto" });
      await reloadRoles();
      void refresh();
      say("Search by meaning is on. The pond reads memories again at the next quiet moment.");
    } catch (e) {
      say(e instanceof Error ? e.message : String(e), false);
    } finally {
      actions.setBusy(false);
    }
  }, [actions, reloadRoles, refresh, say]);

  const retry = useCallback(async () => {
    try {
      await askAgain();
    } catch (e) {
      say(e instanceof Error ? e.message : String(e), false);
    }
  }, [askAgain, say]);

  return {
    rows,
    holder,
    off: memoryIsOff(roles),
    offline,
    line,
    retrying: embedder.retrying,
    useModel,
    turnOn,
    retry,
  };
}
