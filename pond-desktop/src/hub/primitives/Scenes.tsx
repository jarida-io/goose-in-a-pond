import { useState } from "react";
import { HubIco, sunEl, pauseEl, filmEl, focusEl } from "./HubIco";
import { HP_PATHS } from "./icons";
import { useHomeData } from "../state/hubDataStore";

type SceneLayout = "row" | "col" | "grid";

interface ScenesProps {
  layout?: SceneLayout;
}

function getSceneIcon(iconKey: string): string | React.ReactNode {
  switch (iconKey) {
    case "sun":   return sunEl;
    case "moon":  return HP_PATHS.moon;
    case "film":  return filmEl;
    case "away":  return HP_PATHS.away;
    case "focus": return focusEl;
    default:      return HP_PATHS[iconKey as keyof typeof HP_PATHS] ?? "";
  }
}

export function Scenes({ layout = "row" }: ScenesProps) {
  const { scenes } = useHomeData();
  const [active, setActive] = useState(() => scenes[0]?.id ?? "morning");

  return (
    <div className={`scenes scenes--${layout}`}>
      {scenes.map((s) => {
        const isActive = active === s.id;
        return (
          <button
            key={s.id}
            className="scene"
            data-active={isActive}
            onClick={() => setActive(s.id)}
          >
            <span className="scene__icon">
              <HubIco
                d={getSceneIcon(s.icon)}
                size={18}
                color={isActive ? "#fff" : "var(--pp)"}
              />
            </span>
            <span className="scene__name">{s.name}</span>
          </button>
        );
      })}
    </div>
  );
}

// keep pauseEl import satisfied
void pauseEl;
