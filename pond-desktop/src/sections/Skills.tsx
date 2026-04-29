import { useState, useEffect } from "react";
import { Card, CardContent, Button, Input, TextArea, Switch } from "@heroui/react";
import { Plus, Trash2, Zap, Check } from "lucide-react";
import { api } from "../api/PondApiClient";
import type { UserSkill } from "../api/types";

export function Skills() {
  const [skills, setSkills]     = useState<UserSkill[]>([]);
  const [loading, setLoading]   = useState(true);
  const [name, setName]         = useState("");
  const [content, setContent]   = useState("");
  const [showForm, setShowForm] = useState(false);
  const [error, setError]       = useState<string | null>(null);

  function load() {
    setLoading(true);
    api
      .listSkills(true)
      .then(setSkills)
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }

  useEffect(() => {
    load();
  }, []);

  async function add() {
    if (!name.trim() || !content.trim()) return;
    try {
      await api.addSkill(name.trim(), content.trim());
      setName("");
      setContent("");
      setShowForm(false);
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

  async function remove(id: string) {
    try {
      await api.removeSkill(id);
      load();
    } catch (e) {
      setError(String(e));
    }
  }

  return (
    <div className="screen">
      {/* ── Page header ─────────────────────────────────────── */}
      <div className="page-header">
        <h1 className="page-header__title">Skills</h1>
        <div className="page-header__action">
          <Button
            size="sm"
            variant={showForm ? "bordered" : "solid"}
            color="secondary"
            onPress={() => setShowForm((v) => !v)}
            startContent={showForm ? undefined : <Plus size={14} />}
          >
            {showForm ? "Cancel" : "Add Skill"}
          </Button>
        </div>
      </div>

      {/* ── Add form ────────────────────────────────────────── */}
      {showForm && (
        <Card shadow="none" className="giap-card">
          <CardContent>
            <div className="skills-form">
              <Input
                size="sm"
                radius="md"
                variant="bordered"
                label="Name"
                placeholder="Skill name"
                aria-label="Skill name"
                value={name}
                onValueChange={setName}
              />
              <TextArea
                size="sm"
                radius="md"
                variant="bordered"
                label="Instructions"
                placeholder="Skill instructions..."
                aria-label="Skill instructions"
                value={content}
                onChange={(e) => setContent(e.target.value)}
                minRows={3}
              />
              <div className="skills-form__actions">
                <Button
                  size="sm"
                  variant="light"
                  onPress={() => {
                    setShowForm(false);
                    setName("");
                    setContent("");
                  }}
                >
                  Cancel
                </Button>
                <Button
                  size="sm"
                  color="secondary"
                  onPress={add}
                  isDisabled={!name.trim() || !content.trim()}
                  startContent={<Check size={14} />}
                >
                  Save
                </Button>
              </div>
            </div>
          </CardContent>
        </Card>
      )}

      {/* ── Error ───────────────────────────────────────────── */}
      {error && (
        <p style={{ color: "var(--color-destructive)", fontSize: "var(--text-sm)", margin: 0 }}>
          {error}
        </p>
      )}

      {/* ── Skills list ─────────────────────────────────────── */}
      {loading ? (
        <p className="muted-12">Loading...</p>
      ) : skills.length === 0 ? (
        <div className="empty-state">
          <Zap size={32} />
          <span>No skills yet. Add one to get started.</span>
        </div>
      ) : (
        <Card shadow="none" className="giap-card">
          <CardContent className="card-body--list">
            {skills.map((s) => (
              <div key={s.id} className="skill-row">
                <div className="skill-row__icon">
                  <Zap size={16} />
                </div>
                <div style={{ flex: 1, minWidth: 0 }}>
                  <div className="skill-row__name">{s.name}</div>
                  <div className="skill-row__instr">
                    {s.content.length > 80
                      ? s.content.slice(0, 80) + "..."
                      : s.content}
                  </div>
                </div>
                <Switch
                  size="sm"
                  color="secondary"
                  isSelected={s.active}
                  onValueChange={() => toggle(s.id, s.active)}
                  aria-label={`Enable ${s.name}`}
                >
                  <Switch.Control><Switch.Thumb /></Switch.Control>
                </Switch>
                <Button
                  size="sm"
                  variant="light"
                  color="danger"
                  isIconOnly
                  onPress={() => remove(s.id)}
                  aria-label={`Delete ${s.name}`}
                >
                  <Trash2 size={14} />
                </Button>
              </div>
            ))}
          </CardContent>
        </Card>
      )}
    </div>
  );
}
