//! A to-do as the phone writes it: the body of `POST /me/todos` read into a new to-do, and a
//! `PATCH` applied to a kept one. Every refusal names the field.

use super::*;

fn field_str<'a>(body: &'a Value, key: &str) -> Result<Option<&'a str>, HookError> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.as_str())),
        Some(_) => refuse(format!("`{key}` must be a string")),
    }
}

fn text_of(raw: &str) -> Result<String, HookError> {
    let text = raw.trim();
    if text.is_empty() {
        return refuse("`text` must not be empty");
    }
    if text.chars().count() > MAX_TEXT_CHARS {
        return refuse(format!("`text` is at most {MAX_TEXT_CHARS} characters"));
    }
    Ok(text.to_string())
}

fn notes_of(raw: Option<&str>) -> Result<Option<String>, HookError> {
    let notes = raw.map(str::trim).filter(|n| !n.is_empty());
    if notes.is_some_and(|n| n.chars().count() > MAX_NOTES_CHARS) {
        return refuse(format!("`notes` is at most {MAX_NOTES_CHARS} characters"));
    }
    Ok(notes.map(str::to_string))
}

fn list_of(raw: Option<&str>) -> Result<Option<String>, HookError> {
    let list = raw.map(str::trim).filter(|l| !l.is_empty());
    if list.is_some_and(|l| l.chars().count() > MAX_LIST_CHARS) {
        return refuse(format!("`list` is at most {MAX_LIST_CHARS} characters"));
    }
    Ok(list.map(str::to_string))
}

fn priority_of(raw: Option<&str>) -> Result<Option<Priority>, HookError> {
    raw.map(|r| {
        Priority::parse(r).ok_or_else(|| HookError("`priority` is low, normal or high".into()))
    })
    .transpose()
}

/// The steps a body sends, each `{text, done}`, at most [`MAX_STEPS`]; none when it sends null.
fn steps_of(body: &Value) -> Result<Vec<TodoStep>, HookError> {
    let steps = match body.get("steps") {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(steps)) => steps,
        Some(_) => return refuse("`steps` must be a list"),
    };
    if steps.len() > MAX_STEPS {
        return refuse(format!("a to-do has at most {MAX_STEPS} steps"));
    }
    steps
        .iter()
        .enumerate()
        .map(|(i, step)| {
            let text = step
                .get("text")
                .and_then(Value::as_str)
                .map(str::trim)
                .unwrap_or_default();
            if text.is_empty() {
                return refuse(format!("steps[{i}] needs `text`"));
            }
            if text.chars().count() > MAX_STEP_CHARS {
                return refuse(format!(
                    "steps[{i}]: `text` is at most {MAX_STEP_CHARS} characters"
                ));
            }
            let done = match step.get("done") {
                None | Some(Value::Null) => false,
                Some(Value::Bool(done)) => *done,
                Some(_) => return refuse(format!("steps[{i}]: `done` must be true or false")),
            };
            Ok(TodoStep {
                text: text.to_string(),
                done,
            })
        })
        .collect()
}

/// What a body says Goose can do for the to-do, `{label, kind, prompt}`; none when it sends null.
fn goose_can_of(body: &Value) -> Result<Option<GooseCan>, HookError> {
    let can = match body.get("goose_can") {
        None | Some(Value::Null) => return Ok(None),
        Some(can) if can.is_object() => can,
        Some(_) => return refuse("`goose_can` must be an object or null"),
    };
    let part = |key: &str, max: usize| {
        let text = can
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default();
        if text.is_empty() {
            return refuse(format!("`goose_can` needs `{key}`"));
        }
        if text.chars().count() > max {
            return refuse(format!("`goose_can.{key}` is at most {max} characters"));
        }
        Ok(text.to_string())
    };
    let Some(kind) = can
        .get("kind")
        .and_then(Value::as_str)
        .and_then(GooseCanKind::parse)
    else {
        return refuse("`goose_can.kind` is research, chat or watch");
    };
    Ok(Some(GooseCan {
        label: part("label", MAX_GOOSE_LABEL_CHARS)?,
        kind,
        prompt: part("prompt", MAX_GOOSE_PROMPT_CHARS)?,
    }))
}

fn anchor_of(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

fn time_of(key: &str, raw: Option<&str>) -> Result<Option<DateTime<Utc>>, HookError> {
    raw.map(|r| {
        DateTime::parse_from_rfc3339(r.trim())
            .map(|t| t.with_timezone(&Utc))
            .map_err(|_| HookError(format!("`{key}` must be an RFC 3339 time")))
    })
    .transpose()
}

fn bool_of(body: &Value, key: &str) -> Result<Option<bool>, HookError> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => refuse(format!("`{key}` must be true or false")),
    }
}

/// The hooks a body sends, each `{kind, spec}`, at most [`MAX_HOOKS`].
pub fn hook_drafts(body: &Value) -> Result<Option<Vec<(String, Value)>>, HookError> {
    let hooks = match body.get("hooks") {
        None => return Ok(None),
        Some(Value::Null) => return Ok(Some(Vec::new())),
        Some(Value::Array(hooks)) => hooks,
        Some(_) => return refuse("`hooks` must be a list"),
    };
    if hooks.len() > MAX_HOOKS {
        return refuse(format!("a to-do carries at most {MAX_HOOKS} hooks"));
    }
    hooks
        .iter()
        .map(|hook| {
            let Some(kind) = hook.get("kind").and_then(Value::as_str) else {
                return refuse("each hook needs a `kind`");
            };
            Ok((
                kind.to_string(),
                hook.get("spec").cloned().unwrap_or_else(|| json!({})),
            ))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

fn resolve_all(
    drafts: &[(String, Value)],
    catalog: &HookCatalog,
    now: DateTime<Utc>,
    tz: Tz,
) -> Result<Vec<Hook>, HookError> {
    drafts
        .iter()
        .enumerate()
        .map(|(i, (kind, spec))| {
            resolve_hook(kind, spec, catalog, now, tz)
                .map_err(|HookError(why)| HookError(format!("hooks[{i}]: {why}")))
        })
        .collect()
}

/// A new to-do from a `POST /me/todos` body. `ask` defaults to true for a to-do and false for a
/// reminder.
pub fn new_todo(
    owner: &str,
    body: &Value,
    catalog: &HookCatalog,
    source: TodoSource,
    source_ref: Option<String>,
    now: DateTime<Utc>,
    tz: Tz,
) -> Result<Todo, HookError> {
    if !body.is_object() {
        return refuse("the body must be a JSON object");
    }
    let kind = match field_str(body, "kind")? {
        None => TodoKind::Todo,
        Some(raw) => match TodoKind::parse(raw) {
            Some(kind) => kind,
            None => return refuse("`kind` is todo or reminder"),
        },
    };
    let Some(text) = field_str(body, "text")? else {
        return refuse("`text` is required");
    };
    let hooks = resolve_all(&hook_drafts(body)?.unwrap_or_default(), catalog, now, tz)?;
    Ok(Todo {
        id: uuid::Uuid::new_v4().to_string(),
        owner: owner.to_string(),
        kind,
        text: text_of(text)?,
        notes: notes_of(field_str(body, "notes")?)?,
        list: list_of(field_str(body, "list")?)?,
        priority: priority_of(field_str(body, "priority")?)?.unwrap_or_default(),
        steps: steps_of(body)?,
        goose_can: goose_can_of(body)?,
        anchor_id: anchor_of(field_str(body, "anchor_id")?),
        state: TodoState::Open,
        due_at: time_of("due_at", field_str(body, "due_at")?)?,
        ask: bool_of(body, "ask")?.unwrap_or(kind == TodoKind::Todo),
        source,
        source_ref,
        last_asked_at: None,
        asked: 0,
        snoozed_until: None,
        created_at: now,
        updated_at: now,
        done_at: None,
        hooks,
    })
}

/// Applies a `PATCH /me/todos/{id}` body. Returns whether the hooks were rebuilt, which the
/// store must then replace. New hooks, a new due time or reopening start Goose's asking afresh.
pub fn apply_patch(
    todo: &mut Todo,
    body: &Value,
    catalog: &HookCatalog,
    now: DateTime<Utc>,
    tz: Tz,
) -> Result<bool, HookError> {
    if !body.is_object() {
        return refuse("the body must be a JSON object");
    }
    let mut rearmed = false;
    let mut replace_hooks = false;
    if let Some(raw) = field_str(body, "kind")? {
        todo.kind =
            TodoKind::parse(raw).ok_or_else(|| HookError("`kind` is todo or reminder".into()))?;
    }
    if body.get("text").is_some() {
        let Some(text) = field_str(body, "text")? else {
            return refuse("`text` must not be empty");
        };
        todo.text = text_of(text)?;
    }
    if body.get("notes").is_some() {
        todo.notes = notes_of(field_str(body, "notes")?)?;
    }
    if body.get("list").is_some() {
        todo.list = list_of(field_str(body, "list")?)?;
    }
    if let Some(priority) = priority_of(field_str(body, "priority")?)? {
        todo.priority = priority;
    }
    // Ticking the last step closes nothing: the member closes a to-do themselves.
    if body.get("steps").is_some() {
        todo.steps = steps_of(body)?;
    }
    if body.get("goose_can").is_some() {
        todo.goose_can = goose_can_of(body)?;
    }
    if body.get("anchor_id").is_some() {
        todo.anchor_id = anchor_of(field_str(body, "anchor_id")?);
    }
    if body.get("due_at").is_some() {
        let due = time_of("due_at", field_str(body, "due_at")?)?;
        rearmed |= due != todo.due_at;
        todo.due_at = due;
    }
    if let Some(ask) = bool_of(body, "ask")? {
        todo.ask = ask;
    }
    if let Some(raw) = field_str(body, "state")? {
        let Some(state) = TodoState::parse(raw) else {
            return refuse("`state` is open, done or dropped");
        };
        match state {
            TodoState::Done if todo.state != TodoState::Done => todo.done_at = Some(now),
            TodoState::Dropped => todo.done_at = None,
            TodoState::Open if todo.state != TodoState::Open => {
                todo.done_at = None;
                rearmed = true;
                // A repeat that went quiet while closed is due from now, not from its last time.
                for hook in &mut todo.hooks {
                    if matches!(hook.spec, HookSpec::Repeat { .. }) {
                        hook.next_fire_at = hook.spec.first_fire(now, tz);
                        replace_hooks = true;
                    }
                }
            }
            _ => {}
        }
        todo.state = state;
        if state != TodoState::Open {
            todo.snoozed_until = None;
        }
    }
    if let Some(drafts) = hook_drafts(body)? {
        todo.hooks = resolve_all(&drafts, catalog, now, tz)?;
        replace_hooks = true;
        rearmed = true;
    }
    if rearmed && todo.state == TodoState::Open {
        todo.asked = 0;
        todo.snoozed_until = None;
    }
    todo.updated_at = now;
    Ok(replace_hooks)
}

impl Hook {
    pub fn view(&self) -> Value {
        json!({
            "id": self.id,
            "kind": self.spec.kind(),
            "label": self.label,
            "spec": self.spec.to_json(),
            "next_fire_at": self.next_fire_at.map(rfc3339),
            "last_fired_at": self.last_fired_at.map(rfc3339),
        })
    }
}

impl Todo {
    /// The to-do as `GET /me/todos` shows it.
    pub fn view(&self) -> Value {
        json!({
            "id": self.id,
            "kind": self.kind.as_str(),
            "text": self.text,
            "notes": self.notes,
            "list": self.list,
            "priority": self.priority.as_str(),
            "steps": self.steps,
            "goose_can": self.goose_can,
            "anchor_id": self.anchor_id,
            "state": self.state.as_str(),
            "due_at": self.due_at.map(rfc3339),
            "hooks": self.hooks.iter().map(Hook::view).collect::<Vec<_>>(),
            "ask": self.ask,
            "source": self.source.as_str(),
            "last_asked_at": self.last_asked_at.map(rfc3339),
            "asked": self.asked,
            "snoozed_until": self.snoozed_until.map(rfc3339),
            "created_at": rfc3339(self.created_at),
            "done_at": self.done_at.map(rfc3339),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAIROBI: Tz = chrono_tz::Africa::Nairobi;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-09T07:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn create(body: Value) -> Result<Todo, String> {
        new_todo(
            "jerry",
            &body,
            &HookCatalog {
                places: vec![],
                sources: vec![HookSource::presence()],
            },
            TodoSource::You,
            None,
            now(),
            NAIROBI,
        )
        .map_err(|HookError(why)| why)
    }

    #[test]
    fn a_body_becomes_a_todo_with_defaults_by_kind() {
        let todo = create(json!({"text": "  call the garage  "})).unwrap();
        assert_eq!(
            (todo.kind, todo.ask, todo.text.as_str()),
            (TodoKind::Todo, true, "call the garage")
        );
        let reminder = create(json!({
            "kind": "reminder",
            "text": "call Emmanuel",
            "notes": "",
            "due_at": "2026-11-10T18:00:00+03:00",
            "hooks": [{"kind": "event", "spec": {"source_id": "presence:home", "event": "arrive"}}],
        }))
        .unwrap();
        assert_eq!((reminder.kind, reminder.ask), (TodoKind::Reminder, false));
        assert_eq!(reminder.notes, None);
        assert_eq!(rfc3339(reminder.due_at.unwrap()), "2026-11-10T15:00:00Z");
        assert_eq!(reminder.hooks[0].label, "When you get home");
        let view = reminder.view();
        assert_eq!(view["hooks"][0]["kind"], "event");
        assert_eq!(view["source"], "you");
        assert_eq!(view["asked"], 0);
    }

    #[test]
    fn a_bad_field_is_refused_by_name() {
        for (body, says) in [
            (json!([]), "object"),
            (json!({}), "`text` is required"),
            (json!({"text": "   "}), "must not be empty"),
            (json!({"text": 5}), "`text` must be a string"),
            (json!({"text": "x".repeat(501)}), "at most 500"),
            (json!({"text": "x", "kind": "chore"}), "todo or reminder"),
            (json!({"text": "x", "due_at": "Friday"}), "RFC 3339"),
            (json!({"text": "x", "ask": "yes"}), "true or false"),
            (json!({"text": "x", "hooks": {}}), "must be a list"),
            (
                json!({"text": "x", "hooks": [{"spec": {}}]}),
                "needs a `kind`",
            ),
            (
                json!({"text": "x", "hooks": [{"kind": "time", "spec": {"at": "soon"}}]}),
                "hooks[0]",
            ),
            (
                json!({"text": "x", "hooks": vec![json!({"kind": "repeat", "spec": {"every": "day", "at": "09:00"}}); 9]}),
                "at most 8 hooks",
            ),
            (json!({"text": "x", "list": "x".repeat(41)}), "at most 40"),
            (
                json!({"text": "x", "priority": "urgent"}),
                "low, normal or high",
            ),
            (json!({"text": "x", "steps": {}}), "`steps` must be a list"),
            (
                json!({"text": "x", "steps": vec![json!({"text": "a step"}); 13]}),
                "at most 12 steps",
            ),
            (
                json!({"text": "x", "steps": [{"done": true}]}),
                "steps[0] needs `text`",
            ),
            (
                json!({"text": "x", "steps": [{"text": "ok"}, {"text": "x".repeat(121)}]}),
                "steps[1]: `text` is at most 120",
            ),
            (
                json!({"text": "x", "steps": [{"text": "ok", "done": "yes"}]}),
                "steps[0]: `done`",
            ),
            (
                json!({"text": "x", "goose_can": "research"}),
                "an object or null",
            ),
            (
                json!({"text": "x", "goose_can": {"label": "Go", "kind": "book", "prompt": "p"}}),
                "research, chat or watch",
            ),
            (
                json!({"text": "x", "goose_can": {"kind": "chat", "prompt": "p"}}),
                "`goose_can` needs `label`",
            ),
            (
                json!({
                    "text": "x",
                    "goose_can": {"label": "x".repeat(31), "kind": "chat", "prompt": "p"},
                }),
                "`goose_can.label` is at most 30",
            ),
            (json!({"text": "x", "anchor_id": 7}), "`anchor_id` must be"),
        ] {
            let why = create(body.clone()).expect_err(&body.to_string());
            assert!(why.contains(says), "{body}: {why}");
        }
    }

    #[test]
    fn a_list_priority_steps_gooses_offer_and_anchor_are_read_and_shown() {
        let plain = create(json!({"text": "call the garage", "list": "  "})).unwrap();
        assert_eq!(
            (plain.list.clone(), plain.priority, plain.anchor_id.clone()),
            (None, Priority::Normal, None)
        );
        assert!(plain.steps.is_empty() && plain.goose_can.is_none());
        let view = plain.view();
        assert_eq!(
            (&view["list"], &view["goose_can"], &view["anchor_id"]),
            (&Value::Null, &Value::Null, &Value::Null)
        );
        assert_eq!(
            (&view["priority"], &view["steps"]),
            (&json!("normal"), &json!([]))
        );

        let todo = create(json!({
            "text": "Plan the Mombasa trip",
            "list": "  Personal ",
            "priority": "high",
            "steps": [{"text": " Book the train ", "done": true}, {"text": "Pack"}],
            "goose_can": {
                "label": " Find hotels ",
                "kind": "research",
                "prompt": "Hotels in Mombasa under KSh 8,000 a night",
            },
            "anchor_id": " a-1 ",
        }))
        .unwrap();
        assert_eq!(todo.list.as_deref(), Some("Personal"));
        assert_eq!(todo.priority, Priority::High);
        assert_eq!(
            todo.goose_can.as_ref().unwrap().kind,
            GooseCanKind::Research
        );
        let view = todo.view();
        assert_eq!(view["list"], "Personal");
        assert_eq!(view["priority"], "high");
        assert_eq!(
            view["steps"],
            json!([{"text": "Book the train", "done": true}, {"text": "Pack", "done": false}])
        );
        assert_eq!(
            view["goose_can"],
            json!({
                "label": "Find hotels",
                "kind": "research",
                "prompt": "Hotels in Mombasa under KSh 8,000 a night",
            })
        );
        assert_eq!(view["anchor_id"], "a-1");
    }

    #[test]
    fn a_patch_ticks_a_step_and_never_closes_the_todo() {
        let catalog = HookCatalog::default();
        let mut todo = create(json!({
            "text": "Plan the Mombasa trip",
            "list": "Personal",
            "priority": "high",
            "steps": [{"text": "Book the train"}, {"text": "Pack"}],
            "goose_can": {"label": "Find hotels", "kind": "research", "prompt": "Hotels"},
            "anchor_id": "a-1",
        }))
        .unwrap();
        todo.asked = 2;
        let patch = |todo: &mut Todo, body: Value| {
            apply_patch(todo, &body, &catalog, now(), NAIROBI).map_err(|HookError(why)| why)
        };

        let replaced = patch(
            &mut todo,
            json!({"steps": [{"text": "Book the train", "done": true}, {"text": "Pack"}]}),
        )
        .unwrap();
        assert!(!replaced, "steps are not hooks");
        assert_eq!(
            todo.steps.iter().map(|s| s.done).collect::<Vec<_>>(),
            vec![true, false]
        );
        assert_eq!(todo.asked, 2, "ticking a step does not re-arm");

        patch(
            &mut todo,
            json!({"steps": [
                {"text": "Book the train", "done": true},
                {"text": "Pack", "done": true},
            ]}),
        )
        .unwrap();
        assert!(todo.steps.iter().all(|s| s.done));
        assert_eq!(
            (todo.state, todo.done_at),
            (TodoState::Open, None),
            "the member closes it themselves"
        );

        patch(&mut todo, json!({"text": "Plan the coast trip"})).unwrap();
        assert_eq!(todo.steps.len(), 2, "a patch changes only what it names");
        assert_eq!(todo.list.as_deref(), Some("Personal"));
        assert!(todo.goose_can.is_some());

        patch(
            &mut todo,
            json!({"list": "Work", "priority": "low", "goose_can": null, "anchor_id": null}),
        )
        .unwrap();
        assert_eq!(
            (todo.list.as_deref(), todo.priority),
            (Some("Work"), Priority::Low)
        );
        assert_eq!(
            (todo.goose_can.clone(), todo.anchor_id.clone()),
            (None, None)
        );

        patch(
            &mut todo,
            json!({"list": null, "steps": null, "priority": null}),
        )
        .unwrap();
        assert_eq!((todo.list.clone(), todo.steps.len()), (None, 0));
        assert_eq!(todo.priority, Priority::Low, "a null priority leaves it");
        assert!(patch(&mut todo, json!({"priority": "soon"}))
            .unwrap_err()
            .contains("low, normal or high"));
    }

    #[test]
    fn a_patch_changes_only_what_it_names_and_rearms_asking() {
        let mut todo =
            create(json!({"text": "pay the KPLC bill", "notes": "token meter"})).unwrap();
        todo.asked = 3;
        todo.snoozed_until = Some(now());
        let catalog = HookCatalog::default();
        let replaced = apply_patch(
            &mut todo,
            &json!({"text": "pay KPLC"}),
            &catalog,
            now(),
            NAIROBI,
        )
        .unwrap();
        assert!(!replaced);
        assert_eq!(todo.text, "pay KPLC");
        assert_eq!(todo.notes.as_deref(), Some("token meter"));
        assert_eq!(todo.asked, 3, "renaming does not re-arm");

        apply_patch(
            &mut todo,
            &json!({"due_at": "2026-11-10T09:00:00+03:00", "notes": null}),
            &catalog,
            now(),
            NAIROBI,
        )
        .unwrap();
        assert_eq!(
            (todo.asked, todo.snoozed_until, todo.notes.clone()),
            (0, None, None)
        );

        let replaced = apply_patch(
            &mut todo,
            &json!({"hooks": [{"kind": "repeat", "spec": {"every": "day", "at": "07:00"}}]}),
            &catalog,
            now(),
            NAIROBI,
        )
        .unwrap();
        assert!(replaced);
        assert_eq!(todo.hooks[0].label, "Every day at 07:00");
    }

    #[test]
    fn closing_and_reopening_through_a_patch() {
        let catalog = HookCatalog::default();
        let mut todo = create(json!({
            "text": "water the plants",
            "hooks": [{"kind": "repeat", "spec": {"every": "day", "at": "07:00"}}],
        }))
        .unwrap();
        apply_patch(
            &mut todo,
            &json!({"state": "done"}),
            &catalog,
            now(),
            NAIROBI,
        )
        .unwrap();
        assert_eq!((todo.state, todo.done_at), (TodoState::Done, Some(now())));

        let later = now() + Duration::days(3);
        todo.hooks[0].next_fire_at = Some(now());
        todo.asked = 2;
        let replaced = apply_patch(
            &mut todo,
            &json!({"state": "open"}),
            &catalog,
            later,
            NAIROBI,
        )
        .unwrap();
        assert!(replaced, "the repeat's clock moves to after the reopening");
        assert_eq!(
            (todo.state, todo.done_at, todo.asked),
            (TodoState::Open, None, 0)
        );
        assert!(todo.hooks[0].next_fire_at.unwrap() > later);
        assert!(apply_patch(
            &mut todo,
            &json!({"state": "finished"}),
            &catalog,
            later,
            NAIROBI
        )
        .is_err());
    }
}
