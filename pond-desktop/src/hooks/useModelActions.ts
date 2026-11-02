// What a person can do to a model from a Models screen: use it, download it, add picture support,
// pause, resume or stop it. Both surfaces say the same things when they do.

import { useCallback, useState } from "react";
import { api } from "../api/PondApiClient";
import type { ModelEntry } from "../api/types";
import type { UseModels } from "./useModels";
import { controlResult, startedText, type ModelTransfer, type TransferAction } from "../sections/models/modelDownloads";
import { memoryChosenText } from "../sections/models/memoryEmbedder";
import {
  ROLES, type RoleKey, fitsOnlyWithoutPictures, modelLabel, offersPictures,
} from "../sections/models/modelsView";

export interface ModelActions {
  /** One action runs at a time; the buttons rest while it does. */
  busy: boolean;
  setBusy: (busy: boolean) => void;
  /** The tick box on a download, per row: ticked until the household says otherwise, except that it
   *  starts unticked where only the model fits this pond. */
  withPictures: (m: ModelEntry) => boolean;
  setWithPictures: (m: ModelEntry, next: boolean) => void;
  useFor: (m: ModelEntry, role: RoleKey) => Promise<void>;
  download: (m: ModelEntry) => Promise<void>;
  addPictures: (m: ModelEntry) => Promise<void>;
  control: (m: ModelEntry, transfer: ModelTransfer, action: TransferAction) => Promise<void>;
}

const reason = (e: unknown) => (e instanceof Error ? e.message : String(e));

export function useModelActions(
  data: Pick<
    UseModels,
    "memory" | "reloadRoles" | "reloadMemory" | "reloadModels" | "reloadDownloads" | "watchDownloads"
  >,
  say: (text: string, ok?: boolean) => void,
): ModelActions {
  const [busy, setBusy] = useState(false);
  const [choice, setChoice] = useState<Record<string, boolean>>({});
  const { memory, reloadRoles, reloadMemory, reloadModels, reloadDownloads, watchDownloads } = data;

  const attempt = useCallback(
    async (work: () => Promise<void>) => {
      setBusy(true);
      try {
        await work();
      } catch (e) {
        say(reason(e), false);
      } finally {
        setBusy(false);
      }
    },
    [say],
  );

  const withPictures = useCallback(
    (m: ModelEntry) => choice[m.id] ?? !fitsOnlyWithoutPictures(m, memory),
    [choice, memory],
  );
  const setWithPictures = useCallback(
    (m: ModelEntry, next: boolean) => setChoice((c) => ({ ...c, [m.id]: next })),
    [],
  );

  const useFor = useCallback(
    (m: ModelEntry, role: RoleKey) =>
      attempt(async () => {
        // `category` (e.g. "tts_kokoro"), not `provider`: the server builds the lookup id from it,
        // and `provider` is only the list endpoint's group key ("tts").
        await api.activateModel(m.category ?? m.provider, m.name, role);
        await Promise.all([reloadRoles(), reloadMemory()]);
        if (role === "embedding") {
          // The pond fetches a chosen file that is missing, so the page has to start watching for it.
          await reloadDownloads();
          watchDownloads();
          say(memoryChosenText(m));
          return;
        }
        say(`Now using ${modelLabel(m)} for ${ROLES.find((r) => r.key === role)?.label.toLowerCase()}.`);
      }),
    [attempt, reloadRoles, reloadMemory, reloadDownloads, watchDownloads, say],
  );

  const download = useCallback(
    (m: ModelEntry) =>
      attempt(async () => {
        // The pond's own sentence says what it will fetch, picture support included.
        const started = await api.downloadModel(
          m.category ?? m.provider,
          m.name,
          offersPictures(m) ? { pictures: withPictures(m) } : undefined,
        );
        say(startedText(started, modelLabel(m)));
        await Promise.all([reloadModels(), reloadDownloads()]);
        watchDownloads();
      }),
    [attempt, withPictures, reloadModels, reloadDownloads, watchDownloads, say],
  );

  /** Fetches picture support for a model already here. Nothing else ever starts that fetch. */
  const addPictures = useCallback(
    (m: ModelEntry) =>
      attempt(async () => {
        const started = await api.addPictures(m.category ?? m.provider, m.name);
        say(
          started?.status === "already_installed"
            ? `${modelLabel(m)} already has picture support.`
            : startedText(
                started,
                `Picture support for ${modelLabel(m)}`,
                `Adding picture support to ${modelLabel(m)}.`,
              ),
        );
        await Promise.all([reloadModels(), reloadDownloads()]);
        watchDownloads();
      }),
    [attempt, reloadModels, reloadDownloads, watchDownloads, say],
  );

  /** One control for the whole model: the pond moves every part of it, add-on included. */
  const control = useCallback(
    (m: ModelEntry, transfer: ModelTransfer, action: TransferAction) =>
      attempt(async () => {
        await api.controlModelDownload(transfer.modelId, action);
        say(controlResult(action, modelLabel(m), transfer.resumable));
        await reloadDownloads();
        if (action === "resume") watchDownloads();
      }),
    [attempt, reloadDownloads, watchDownloads, say],
  );

  return { busy, setBusy, withPictures, setWithPictures, useFor, download, addPictures, control };
}
