// ─── Routines: the shape, and a design fixture ─────────────────
// Ported from goose-hub-settings.jsx ROUTINE_DETAIL constant.
// Routines are on-demand scenes/macros that execute immediately on tap.
// They are NOT time-triggered schedules — see Phase 6 notes in scratchpad.md.
//
// `ROUTINES` below is a FIXTURE and must never reach a household. It used to:
// `hubDataStore` seeded it and returned it whenever the recipe list came back
// empty, so a fresh pond — and any pond whose server was unreachable — listed
// five routines it did not have, in the drawer and on Routines, each offering
// to run. Tapping one wrote it into the household's real recipes and sent its
// prompt to the agent. The store now maps recipes and only recipes; the one
// remaining consumer is `sections/Schedules.tsx`, which looks a `RoutineId` up
// by id and finds nothing for a recipe-derived routine, which is correct.
//
// Note the `time` strings in particular. Recipes carry no schedule, so every
// one of these is a claim no record in the pond can support.

import { sunEl, filmEl, focusEl } from "../primitives/HubIco";
import { HP_PATHS } from "../primitives/icons";
import type React from "react";

export type RoutineId = "morning" | "night" | "movie" | "away" | "focus";

export interface RoutineDetail {
  id: RoutineId;
  name: string;
  /**
   * SVG icon content — either a path string (d attribute) or a ReactNode for
   * multi-path compound icons. HubIco handles both via typeof check.
   */
  iconPath: string | React.ReactNode;
  /** Primary accent color (for Run button border + filled state) */
  color: string;
  /** Gradient CSS string (for icon square background) */
  bg: string;
  /** Chip labels describing what the routine does */
  does: string[];
  /** Human-readable time/trigger meta */
  time: string;
  /** Prompt sent to Goose when this routine runs. Used to seed the recipe on first run. */
  prompt: string;
}

export const ROUTINES: RoutineDetail[] = [
  {
    id: "morning",
    name: "Good Morning",
    iconPath: sunEl,
    color: "#F59E0B",
    bg: "linear-gradient(150deg,#FCD34D,#F59E0B)",
    does: ["Lights to 60%", "Heat to 70°", "Brew coffee", "Read briefing"],
    time: "7:00 AM · weekdays",
    prompt: "Run the Good Morning routine: set lights to 60% brightness, set thermostat to 70°F, start brewing coffee, and read the morning news briefing.",
  },
  {
    id: "night",
    name: "Good Night",
    iconPath: HP_PATHS.moon,
    color: "#6366F1",
    bg: "linear-gradient(150deg,#818CF8,#4F46E5)",
    does: ["Lock all doors", "Lights off", "Heat to 66°", "Arm security"],
    time: "11:00 PM · daily",
    prompt: "Run the Good Night routine: lock all doors, turn off all lights, set thermostat to 66°F, and arm the security system.",
  },
  {
    id: "movie",
    name: "Movie Time",
    iconPath: filmEl,
    color: "#7C3AED",
    bg: "linear-gradient(150deg,#A78BFA,#7C3AED)",
    does: ["Dim to 20%", "Close blinds", "TV on", "Mute notifications"],
    time: "8:00 PM · evenings",
    prompt: "Run the Movie Time routine: dim the lights to 20%, close the blinds, turn on the TV, and mute all notifications.",
  },
  {
    id: "away",
    name: "Away",
    iconPath: HP_PATHS.away,
    color: "#0D9488",
    bg: "linear-gradient(150deg,#2DD4BF,#0D9488)",
    does: ["Lock up", "Eco climate", "Cameras armed", "Lights off"],
    time: "When leaving home",
    prompt: "Run the Away routine: lock all doors and windows, set climate to eco mode, arm all cameras, and turn off all lights.",
  },
  {
    id: "focus",
    name: "Focus",
    iconPath: focusEl,
    color: "#EC4899",
    bg: "linear-gradient(150deg,#F472B6,#DB2777)",
    does: ["Do not disturb", "Desk lamp on", "Lo-fi playlist", "Heat to 71°"],
    time: "9:00 AM · work days",
    prompt: "Run the Focus routine: enable do not disturb mode, turn on the desk lamp, start a lo-fi music playlist, and set thermostat to 71°F.",
  },
];
