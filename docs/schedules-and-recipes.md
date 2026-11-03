# Schedules and recipes

A **schedule** decides **when** the Pond does something on its own. A **recipe** is a saved task:
**what** to do and which tools to use, started by someone. The desktop shows the same summary under
**Schedule or recipe?** on both screens.

## What each one is

**Schedule** (the Schedules screen): a prompt and a time. At that time, once or on repeat, the Pond
runs the prompt as if someone had asked it in chat, with nobody there. It can speak the answer when
quiet hours allow, and it can ask before running a time it missed while the Pond was off. Example:
"Every weekday at 07:00, tell me the weather and today's reminders."

**Recipe** (the Recipes screen): a reusable task with a title, a prompt, optional fill-in values
(`{{city}}`) and, optionally, the tools it may use. It runs only when someone starts it: its Run
button, or a routine on the Schedules screen. Example: "Morning brief for {{city}}", using only
Weather and Reminders and schedules.

**Agent** (also on the Recipes screen): a recipe with its own triggers, set under **When it runs**,
such as a time or an event in the house.

## How they differ

| | Schedule | Recipe |
|---|---|---|
| Answers | When | What |
| Starts | By itself, at its time | When someone starts it |
| Holds | A prompt, a time, a timezone | A prompt, fill-in values, chosen tools |
| Fill-in values | No | Yes, asked for before it runs |
| Choosing tools | No: every tool turned on, as in chat | Optional: tick some and it gets only those |
| Runs for | The whole household, with nobody there | Whoever started it |
| Where | Schedules screen | Recipes screen; routines on the Schedules screen |

## Which tools each can use

Only tools turned on in Settings → Tools are ever used. Within those:

| Tool | Schedule | Recipe | Agent |
|---|---|---|---|
| Memories | Yes | If ticked | If chosen |
| Date, time and notifications | Yes | Only if none ticked | Never |
| Reminders and schedules | Yes | If ticked | Never |
| Weather | Yes | If ticked | If chosen |
| Look things up and search the web | Yes | Only if none ticked | If chosen |
| See the devices in the house | Yes | If ticked | If chosen |
| Switch devices on and off | Yes | If ticked | Never |
| Sensor readings | Yes | Only if none ticked | If chosen |
| Personal context | Yes | Only if none ticked | If chosen |
| Cameras | Yes | If ticked | Never |

- **Yes:** a schedule gets every tool a chat gets, and the Pond picks the ones each run needs.
- **If ticked / Only if none ticked:** a recipe with no tool ticked gets the same tools as a chat.
  Once any is ticked, it gets only the ticked ones, so tools without a box are left out.
- **If chosen / Never:** an agent runs with no one watching, so it is never given tools that act on
  the house, notify people, schedule more work or watch the cameras.

## How they work together

- **A routine is a recipe one tap away.** The routines at the top of the Schedules screen are
  recipes: tapping one runs it now.
- **To run a recipe at a set time, make it an agent** and give it a time under **When it runs**.
- **Make a schedule for a plain prompt at a time; make a recipe for a task you repeat** with
  different values or a fixed set of tools.
- **A recipe can make schedules:** tick Reminders and schedules, and a recipe like "Remind me
  to {{task}} at {{time}}" sets one.

The table lives in code in `pond-desktop/src/sections/toolReach.ts`. A recipe's tool names map to
tool groups in `recipe_extension_to_tool_group` (`crates/pond-api/src/routes.rs`), and the groups
an agent is never given are `WITHHELD_FROM_AGENTS` (`pond-desktop/src/agents/agentToolGroups.ts`).
