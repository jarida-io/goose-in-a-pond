import React, { useState } from "react";
import "./routines.css";
import { HubIco } from "../primitives/HubIco";
import { HP_PATHS } from "../primitives/icons";
import type { RoutineId } from "../data/routines";
import { useRoutines } from "../state/hubDataStore";
import { api } from "../../api/PondApiClient";
import { RecipeBuilderModal } from "./RecipeBuilderModal";

async function executeRoutine(name: string): Promise<void> {
  try {
    // Drain the run's event stream; the events aren't shown.
    for await (const _ of api.runRecipe(name)) {
      void _;
    }
  } catch {
    // Best-effort: surfacing errors here would block the visual "Running" toast.
  }
}

// ─── RoutinesView ──────────────────────────────────────────────
// One-tap cards for backend AgentRecipes; runs are fire-and-forget behind the "Running…" toast.

export function RoutinesView() {
  const routines = useRoutines();
  const [running, setRunning] = useState<RoutineId | null>(null);
  const [builderOpen, setBuilderOpen] = useState(false);

  const handleRun = (id: RoutineId, name: string) => {
    setRunning(id);
    void executeRoutine(name);
    setTimeout(() => setRunning(null), 1600);
  };

  const openBuilder = () => setBuilderOpen(true);
  const closeBuilder = () => setBuilderOpen(false);

  return (
    <div className="rt">
      <header className="view-head">
        <div>
          <h1 className="view-title">Routines</h1>
          <p className="view-sub">
            One tap to set the whole house. Goose runs these for you.
          </p>
        </div>
        <button className="primary-btn" onClick={openBuilder}>
          <HubIco d={HP_PATHS.plus} size={16} color="#fff" />
          + New routine
        </button>
      </header>

      <div className="rt__grid">
        {routines.map((routine) => {
          const isRunning = running === routine.id;
          return (
            <div key={routine.id} className="rt-card">
              {/* top: icon + name + time */}
              <div className="rt-card__top">
                <span
                  className="rt-card__icon"
                  style={{ background: routine.bg }}
                >
                  <HubIco d={routine.iconPath} size={22} color="#fff" sw={2} />
                </span>
                <div>
                  <div className="rt-card__name">{routine.name}</div>
                  <div className="rt-card__time">{routine.time}</div>
                </div>
              </div>

              {/* does chips */}
              <div className="rt-card__does">
                {routine.does.map((chip) => (
                  <span key={chip} className="rt-card__chip">
                    {chip}
                  </span>
                ))}
              </div>

              {/* run button */}
              <button
                className="rt-card__run"
                data-running={isRunning}
                style={
                  isRunning
                    ? {
                        background: routine.color,
                        borderColor: routine.color,
                        color: "#fff",
                      }
                    : { color: routine.color }
                }
                onClick={() => handleRun(routine.id, routine.name)}
              >
                {isRunning ? (
                  <>
                    <HubIco d={HP_PATHS.check} size={15} color="#fff" sw={3} />
                    Running...
                  </>
                ) : (
                  <>
                    <HubIco
                      d={HP_PATHS.play}
                      size={14}
                      color={routine.color}
                      fill={routine.color}
                    />
                    Run now
                  </>
                )}
              </button>
            </div>
          );
        })}

        {/* dashed "Create a routine" placeholder tile */}
        <button className="rt-card rt-card--new" onClick={openBuilder}>
          <HubIco d={HP_PATHS.plus} size={26} color="#9A8FB8" />
          <span>Create a routine</span>
        </button>
      </div>

      {builderOpen && <RecipeBuilderModal onClose={closeBuilder} />}
    </div>
  );
}
