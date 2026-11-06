import { WITHHELD_FROM_AGENTS } from "../agents/agentToolGroups";
import { TOOL_REACH } from "./toolReach";

/** What a schedule and a recipe are, and which tools each can use. Shown on both screens. */
export function ScheduleOrRecipe() {
  return (
    <details className="card sched-or-recipe">
      <summary>Schedule or recipe?</summary>
      <dl className="sched-or-recipe__terms">
        <dt>Schedule</dt>
        <dd><strong>When.</strong> Runs a prompt by itself at a set time, once or on repeat, with nobody asking.</dd>
        <dt>Recipe</dt>
        <dd><strong>What.</strong> A saved task you start yourself, with fill-in values and the tools it may use. Routines are recipes.</dd>
        <dt>Together</dt>
        <dd>An agent is a recipe with its own schedule: open it under Recipes and set &ldquo;When it runs&rdquo;.</dd>
      </dl>
      <table className="sched-or-recipe__tools">
        <caption>Which tools each can use</caption>
        <thead>
          <tr><th scope="col">Tool</th><th scope="col">Schedule</th><th scope="col">Recipe</th><th scope="col">Agent</th></tr>
        </thead>
        <tbody>
          {TOOL_REACH.map((t) => (
            <tr key={t.group}>
              <th scope="row">{t.label}</th>
              <td>Yes</td>
              <td>{t.recipe ? "If ticked" : "Only if none ticked"}</td>
              <td>{WITHHELD_FROM_AGENTS.includes(t.group) ? "Never" : "If chosen"}</td>
            </tr>
          ))}
        </tbody>
      </table>
      <p className="muted-12">
        Only tools turned on in Settings. A schedule, and a recipe with no tool ticked, get the same tools
        as a chat, and the Pond picks the ones each run needs. Tick any tool on a recipe and it gets only those.
      </p>
    </details>
  );
}
