// The specialists, each with a switch that applies at once; one Goose offered says why.

import { useState } from "react";
import { InkCard, InkSwitch } from "@jarida/ink/react";
import { api } from "../api/PondApiClient";
import type { Specialist } from "../api/types";
import {
  LOADING_SPECIALISTS,
  NO_SPECIALISTS,
  OFFERED_PREFIX,
  SPECIALISTS_HEADING,
  SPECIALISTS_INTRO,
  specialistStateWords,
  type Variant,
} from "./myGooseWords";
import { problemWords } from "./problems";
import { useLoaded } from "./useLoaded";
import { ActionProblem, LoadProblem, Loading } from "./MyGooseParts";

export function SpecialistsPanel({ variant, say }: { variant: Variant; say: (text: string) => void }) {
  const agents = useLoaded(() => api.listSpecialists());
  const [busy, setBusy] = useState<string | null>(null);
  const [problem, setProblem] = useState<{ id: string; text: string } | null>(null);

  async function flip(agent: Specialist, on: boolean) {
    setBusy(agent.id);
    setProblem(null);
    try {
      agents.replace(await api.switchSpecialist(agent.id, on));
      say(`${agent.title} ${on ? "turned on" : "turned off"}.`);
    } catch (e) {
      setProblem({ id: agent.id, text: problemWords(e) });
    } finally {
      setBusy(null);
    }
  }

  const list = agents.state.status === "ready" ? agents.state.value : [];

  return (
    <section className="mg-panel" aria-label={SPECIALISTS_HEADING}>
      <p className="mg__intro">{SPECIALISTS_INTRO}</p>
      {agents.state.status === "loading" ? <Loading>{LOADING_SPECIALISTS}</Loading> : null}
      {agents.state.status === "failed" ? <LoadProblem problem={agents.state.problem} onRetry={agents.retry} variant={variant} /> : null}
      {agents.state.status === "ready" && list.length === 0 ? <p className="mg__note">{NO_SPECIALISTS}</p> : null}
      <ul className="mg-list" aria-label={SPECIALISTS_HEADING}>
        {list.map((agent) => (
          <li key={agent.id}>
            <InkCard raised={agent.state === "offered"} className="mg-agent">
              <InkSwitch label={agent.title} checked={agent.state === "on"} disabled={busy !== null} onChange={(on) => void flip(agent, on)}>
                <span className="mg-agent__title">{agent.title}</span>
              </InkSwitch>
              <p className="mg-agent__about">{agent.about}</p>
              <p className="mg-agent__state">{specialistStateWords(agent.state)}</p>
              {agent.state === "offered" && agent.why.trim() !== "" ? (
                <p className="mg-agent__why">
                  {OFFERED_PREFIX} {agent.why}.
                </p>
              ) : null}
              <ActionProblem text={problem?.id === agent.id ? problem.text : null} />
            </InkCard>
          </li>
        ))}
      </ul>
    </section>
  );
}
