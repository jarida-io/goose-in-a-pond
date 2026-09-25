import { useState, useEffect } from "react";
import { Card, CardContent, Button, Input, Switch } from "@heroui/react";
import {
  Plus, Trash2, Sparkles, Check, Pencil,
  Bell, Clock, Calendar, MessageCircle, Home, Shield, Lightbulb, Wrench, Music,
} from "lucide-react";
import { api } from "../api/PondApiClient";
import { PageHeader, useConfirm } from "../components/shared";
import type { UserSkill } from "../api/types";

// ── Icon set — a small curated palette, same pattern as Sidebar's NAV_ICONS ──
const SKILL_ICONS: Record<string, React.ElementType> = {
  sparkles: Sparkles,
  bell: Bell,
  clock: Clock,
  calendar: Calendar,
  message: MessageCircle,
  home: Home,
  shield: Shield,
  lightbulb: Lightbulb,
  wrench: Wrench,
  music: Music,
};
const SKILL_ICON_KEYS = Object.keys(SKILL_ICONS);

// ── Icon inference from the skill's name; order matters, first match wins ──
const ICON_KEYWORDS: Array<[string, string[]]> = [
  ["wrench",    ["setting", "config", "tool", "repair", "maintenance", "fix"]],
  ["bell",      ["remind", "alert", "notify", "notification"]],
  ["clock",     ["time", "timer", "alarm", "wake"]],
  ["calendar",  ["schedule", "calendar", "appointment", "event", "meeting"]],
  ["message",   ["chat", "brief", "summary", "news", "message", "conversation"]],
  ["home",      ["home", "house", "household", "family"]],
  ["shield",    ["secur", "safety", "protect", "lock", "guard"]],
  ["music",     ["music", "song", "playlist", "audio"]],
  ["lightbulb", ["idea", "automat", "light", "smart"]],
];

function inferSkillIcon(name: string): string {
  const lower = name.toLowerCase();
  for (const [icon, keywords] of ICON_KEYWORDS) {
    if (keywords.some((k) => lower.includes(k))) return icon;
  }
  return "sparkles";
}

function SkillIcon({ icon, name, size = 16 }: { icon: string; name?: string; size?: number }) {
  // "sparkles" is the untouched default, so infer from the name for display (no migration needed).
  const key = icon === "sparkles" && name ? inferSkillIcon(name) : icon;
  const Icon = SKILL_ICONS[key] ?? Sparkles;
  return <Icon size={size} />;
}

export function Skills() {
  const confirm = useConfirm();
  const [skills, setSkills] = useState<UserSkill[]>([]);
  const [loading, setLoading] = useState(true);
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [icon, setIcon] = useState("sparkles");
  // Icon deliberately chosen (picker click, or a saved real icon); until then the name re-infers it.
  const [iconTouched, setIconTouched] = useState(false);
  const [content, setContent] = useState("");
  const [showForm, setShowForm] = useState(false);
  const [editingSkill, setEditingSkill] = useState<UserSkill | null>(null);
  const [error, setError] = useState<string | null>(null);

  function load() {
    setLoading(true);
    api
      .listSkills(true)
      .then((res) => setSkills(Array.isArray(res) ? res : []))
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }

  useEffect(() => { load(); }, []);

  function resetForm() {
    setShowForm(false);
    setEditingSkill(null);
    setName("");
    setDescription("");
    setIcon("sparkles");
    setIconTouched(false);
    setContent("");
  }

  function startEdit(s: UserSkill) {
    setEditingSkill(s);
    setName(s.name);
    setDescription(s.description);
    // Still on the "sparkles" default: infer, and keep re-inferring on rename like a new skill.
    const savedIcon = s.icon || "sparkles";
    setIcon(savedIcon === "sparkles" ? inferSkillIcon(s.name) : savedIcon);
    setIconTouched(savedIcon !== "sparkles");
    setContent(s.content);
    setShowForm(true);
  }

  function handleNameChange(v: string) {
    setName(v);
    if (!iconTouched) setIcon(inferSkillIcon(v));
  }

  function pickIcon(key: string) {
    setIcon(key);
    setIconTouched(true);
  }

  async function save() {
    if (!name.trim() || !description.trim() || !content.trim()) return;
    try {
      if (editingSkill) {
        await api.updateSkill(editingSkill.id, {
          name: name.trim(),
          description: description.trim(),
          icon,
          content: content.trim(),
        });
      } else {
        await api.addSkill(name.trim(), description.trim(), content.trim(), icon);
      }
      resetForm();
      load();
    } catch (e) {
      setError(String(e));
    }
  }

  async function toggle(id: string, currentActive: boolean) {
    try {
      await api.toggleSkill(id, currentActive);
      load();
    } catch (e) {
      setError(String(e));
    }
  }

  async function remove(id: string, skillName: string) {
    if (!await confirm(`Delete skill "${skillName}"? This cannot be undone.`, { title: "Delete Skill", confirmLabel: "Delete", destructive: true })) return;
    try {
      await api.removeSkill(id);
      load();
    } catch (e) {
      setError(String(e));
    }
  }

  return (
    <div className="screen screen--skills">
      <PageHeader
        title="Skills"
        action={
          <Button
            variant="secondary"
            onPress={() => (showForm ? resetForm() : setShowForm(true))}
          >
            {showForm ? null : <Plus size={14} />} {showForm ? "Cancel" : "Add Skill"}
          </Button>
        }
      />

      {showForm && (
        <Card className="card">
          <CardContent>
            <div className="skills-form">
              <h3 className="skills-form__title">{editingSkill ? "Edit skill" : "New skill"}</h3>
              <label htmlFor="skill-name" className="ext-form-label">Skill name</label>
              <Input
                id="skill-name"
                placeholder="e.g. Task Reminder"
                value={name}
                onChange={(e) => handleNameChange(e.target.value)}
              />
              <label htmlFor="skill-description" className="ext-form-label">Description</label>
              <Input
                id="skill-description"
                placeholder='When should this activate? e.g. "Creates reminders when asked to be reminded of something"'
                value={description}
                onChange={(e) => setDescription(e.target.value)}
              />
              <label className="ext-form-label">Icon</label>
              <div className="skills-form__icons">
                {SKILL_ICON_KEYS.map((key) => (
                  <button
                    key={key}
                    type="button"
                    className={`skills-form__icon-btn${icon === key ? " skills-form__icon-btn--selected" : ""}`}
                    aria-label={key}
                    aria-pressed={icon === key}
                    onClick={() => pickIcon(key)}
                  >
                    <SkillIcon icon={key} />
                  </button>
                ))}
              </div>
              <label htmlFor="skill-content" className="ext-form-label">Instructions</label>
              <textarea
                id="skill-content"
                className="skills-form__textarea"
                placeholder="The full instructions Pond follows once this skill activates..."
                value={content}
                onChange={(e) => setContent(e.target.value)}
                rows={4}
              />
              <div className="skills-form__actions">
                <Button
                  variant="ghost"
                  onPress={resetForm}
                >
                  Cancel
                </Button>
                <Button
                  variant="secondary"
                  onPress={save}
                  isDisabled={!name.trim() || !description.trim() || !content.trim()}
                >
                  <Check size={14} /> {editingSkill ? "Update skill" : "Save skill"}
                </Button>
              </div>
            </div>
          </CardContent>
        </Card>
      )}

      {error && (
        <p className="text-error text-error--sm">{error}</p>
      )}

      {loading ? (
        <p className="muted-12">Loading...</p>
      ) : skills.length === 0 && !showForm ? (
        <Card className="card">
          <CardContent className="card-body--list">
            <div className="empty-state">
              <Sparkles size={20} />
              <span>No skills yet. Add one to teach Pond a new capability.</span>
            </div>
          </CardContent>
        </Card>
      ) : skills.length > 0 ? (
        <div className="skills-grid">
          {skills.map((s) => (
            <div key={s.id} className={`ink-edge ink-card skill-card${!s.active ? " skill-card--inactive" : ""}`}>
              <div className="skill-card__top">
                <span className="skill-card__icon">
                  <SkillIcon icon={s.icon} name={s.name} />
                </span>
                <Switch
                  size="sm"
                  isSelected={s.active}
                  onChange={() => toggle(s.id, s.active)}
                  aria-label={`Enable ${s.name}`}
                >
                  <Switch.Content><Switch.Control><Switch.Thumb /></Switch.Control></Switch.Content>
                </Switch>
              </div>
              <h3 className="skill-card__title">{s.name}</h3>
              <p className="skill-card__desc">
                {s.description || <span className="muted">No description</span>}
              </p>
              <div className="skill-card__actions">
                <button
                  type="button"
                  className="skill-card__action-btn"
                  onClick={() => startEdit(s)}
                  aria-label={`Edit ${s.name}`}
                >
                  <Pencil size={14} />
                </button>
                <button
                  type="button"
                  className="skill-card__action-btn skill-card__action-btn--danger"
                  onClick={() => remove(s.id, s.name)}
                  aria-label={`Delete ${s.name}`}
                >
                  <Trash2 size={14} />
                </button>
              </div>
            </div>
          ))}
        </div>
      ) : null}
    </div>
  );
}
