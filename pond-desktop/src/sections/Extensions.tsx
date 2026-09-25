import { useState, useEffect, useCallback, useRef } from "react";
import {
  Button,
  Card,
  CardContent,
  Switch,
} from "@heroui/react";
import {
  Puzzle,
  Trash2,
  ChevronDown,
  ChevronUp,
  Plus,
  RefreshCw,
  Wrench,
  Terminal,
  Download,
  Check,
  Star,
  AlertCircle,
  Store,
  KeyRound,
  Eye,
  EyeOff,
  X,
  Music,
  ExternalLink,
} from "lucide-react";
import { api } from "../api/PondApiClient";
import { invoke, isDesktopShell } from "../shell";
import { useAppState } from "../state/AppContext";
import { useConfirm, ErrorBanner } from "../components/shared";
import type { Extension, AddExtensionRequest, MarketplaceExtension, SecretRequirement, AgentTool } from "../api/types";

/** Open a URL in the real browser. In the shell it must go via the main process:
 *  `window.open` on an app:// page opens another in-app window. */
async function openExternal(url: string) {
  if (isDesktopShell()) {
    await invoke("open_external", { url });
    return;
  }
  window.open(url, "_blank", "noopener,noreferrer");
}

// ── Secret Config Modal ───────────────────────────────────────

type SecretModalMode = "install" | "edit";

interface SecretConfigModalProps {
  ext: MarketplaceExtension;
  mode: SecretModalMode;
  /** fulfilled map for edit mode — key → already stored */
  fulfilledMap?: Record<string, boolean>;
  onClose: () => void;
  onComplete: (secrets: Record<string, string>) => Promise<void>;
}

/** One password field with show/hide toggle and inline error. */
function SecretField({
  req,
  value,
  onChange,
  error,
  fulfilled,
  disabled,
}: {
  req: SecretRequirement;
  value: string;
  onChange: (v: string) => void;
  error?: string;
  fulfilled?: boolean;
  disabled: boolean;
}) {
  const [visible, setVisible] = useState(false);

  return (
    <div className="secret-modal__field">
      <label className="secret-modal__field-label" htmlFor={`secret-${req.key}`}>
        {req.display_name}
        {req.required && <span className="secret-modal__required-badge">Required</span>}
        {fulfilled && !value && (
          <span className="secret-modal__fulfilled-indicator">
            <Check size={11} strokeWidth={2.5} />
            Saved
          </span>
        )}
      </label>

      <div className="secret-modal__input-wrap">
        <input
          id={`secret-${req.key}`}
          type={visible ? "text" : "password"}
          className={`secret-modal__input${fulfilled && !value ? " secret-modal__input--fulfilled" : ""}`}
          value={value}
          onChange={(e) => onChange(e.target.value)}
          placeholder={fulfilled ? "Leave blank to keep existing value" : `Enter your ${req.display_name}`}
          autoComplete="off"
          autoCorrect="off"
          autoCapitalize="off"
          spellCheck={false}
          disabled={disabled}
        />
        <button
          type="button"
          className="secret-modal__input-toggle"
          onClick={() => setVisible((v) => !v)}
          aria-label={visible ? "Hide value" : "Show value"}
          tabIndex={-1}
        >
          {visible
            ? <EyeOff size={13} strokeWidth={1.8} />
            : <Eye size={13} strokeWidth={1.8} />
          }
        </button>
      </div>

      {req.description && !error && (
        <p className="secret-modal__field-hint">{req.description}</p>
      )}
      {error && (
        <p className="secret-modal__field-error">{error}</p>
      )}
    </div>
  );
}

/** Browser hand-off timeout; generous because the user may have to log in and pick an account. */
const OAUTH_POLL_TIMEOUT_MS = 5 * 60 * 1000;

/** OAuth sign-in block for a single oauth_flow requirement. */
function OAuthBlock({
  req,
  extensionId,
  alreadyAuthorized = false,
  onAuthorized,
  disabled,
}: {
  req: SecretRequirement;
  extensionId: string;
  /** A token is already stored. Context only: it never counts as a completed flow (it may be stale). */
  alreadyAuthorized?: boolean;
  onAuthorized: () => void;
  disabled: boolean;
}) {
  const [oauthState, setOauthState] = useState<"idle" | "polling" | "done" | "reconnecting">("idle");
  const [error, setError] = useState<string | null>(null);
  const pollRef = useRef<ReturnType<typeof setInterval> | null>(null);
  const reconnectTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  function stopPoll() {
    if (pollRef.current !== null) {
      clearInterval(pollRef.current);
      pollRef.current = null;
    }
  }

  useEffect(() => () => {
    stopPoll();
    if (reconnectTimerRef.current !== null) {
      clearTimeout(reconnectTimerRef.current);
    }
  }, []);

  async function handleSignIn() {
    setError(null);
    setOauthState("polling");
    try {
      const { auth_url, state } = await api.initiateOAuth(req.key, extensionId);
      openExternal(auth_url);

      // Poll THIS flow by its state nonce: on re-authorisation the store already holds the old
      // token, so watching it would report success before the user signs in.
      const startedAt = Date.now();
      pollRef.current = setInterval(async () => {
        try {
          const { status, error: flowError } = await api.getOAuthStatus(state);

          if (status === "completed") {
            stopPoll();
            setOauthState("done");
            onAuthorized();
            reconnectTimerRef.current = setTimeout(() => {
              setOauthState("reconnecting");
            }, 800);
            return;
          }

          if (status === "failed") {
            stopPoll();
            setOauthState("idle");
            setError(flowError || "Sign-in failed. Please try again.");
            return;
          }

          // "pending" and "unknown" both mean keep waiting: a server restarted mid-flow reports "unknown".
          if (Date.now() - startedAt > OAUTH_POLL_TIMEOUT_MS) {
            stopPoll();
            setOauthState("idle");
            setError("Timed out waiting for sign-in to complete. Please try again.");
          }
        } catch {
          // ignore transient check failures, keep polling
        }
      }, 2000);
    } catch (err) {
      setOauthState("idle");
      setError(err instanceof Error ? err.message : String(err));
    }
  }

  const isMusic = req.key.toLowerCase().includes("spotify") || req.display_name.toLowerCase().includes("spotify");
  const BtnIcon = isMusic ? Music : KeyRound;

  return (
    <div className="secret-modal__oauth-block">
      <p className="secret-modal__oauth-desc">{req.description}</p>

      {oauthState === "idle" && (
        <>
          {alreadyAuthorized && (
            <p className="secret-modal__oauth-note">
              Already connected. Sign in again to switch account, or if playback stopped working.
            </p>
          )}
          <button
            type="button"
            className="secret-modal__oauth-btn"
            onClick={handleSignIn}
            disabled={disabled}
          >
            <BtnIcon size={13} strokeWidth={1.8} />
            {alreadyAuthorized ? "Sign in again" : `Sign in with ${req.display_name}`}
            <ExternalLink size={11} strokeWidth={2} />
          </button>
        </>
      )}

      {oauthState === "polling" && (
        <>
          <div className="secret-modal__oauth-polling">
            <div className="secret-modal__oauth-spinner" />
            <span>Waiting for authorisation in browser…</span>
          </div>
          <button
            type="button"
            className="secret-modal__oauth-btn secret-modal__oauth-btn--mt"
            onClick={handleSignIn}
            disabled={disabled}
          >
            <ExternalLink size={11} strokeWidth={2} />
            Reopen sign-in window
          </button>
        </>
      )}

      {oauthState === "done" && (
        <div className="secret-modal__fulfilled-indicator">
          <Check size={13} strokeWidth={2.5} />
          Authorised
        </div>
      )}

      {oauthState === "reconnecting" && (
        <div className="secret-modal__reconnecting">
          <div className="secret-modal__oauth-spinner" />
          <span>Reconnecting extension…</span>
        </div>
      )}

      {error && <p className="secret-modal__field-error">{error}</p>}
    </div>
  );
}

function SecretConfigModal({
  ext,
  mode,
  fulfilledMap = {},
  onClose,
  onComplete,
}: SecretConfigModalProps) {
  const apiKeyReqs = ext.required_secrets.filter((r) => r.kind === "api_key" || r.kind === "generic");
  const oauthReqs = ext.required_secrets.filter((r) => r.kind === "oauth_flow");

  const [values, setValues] = useState<Record<string, string>>(() =>
    Object.fromEntries(apiKeyReqs.map((r) => [r.key, ""])),
  );
  const [fieldErrors, setFieldErrors] = useState<Record<string, string>>({});
  const [globalError, setGlobalError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [reconnecting, setReconnecting] = useState(false);

  // Authorisations completed in this modal session, not stored tokens: seeding from
  // fulfilledMap would auto-complete an OAuth-only edit on mount and block re-authorising.
  const [oauthDone, setOauthDone] = useState<Record<string, boolean>>(() =>
    Object.fromEntries(oauthReqs.map((r) => [r.key, false])),
  );

  // OAuth-only modals auto-close once every flow is done.
  const onlyOauth = apiKeyReqs.length === 0 && oauthReqs.length > 0;
  const allOauthDone = oauthReqs.length > 0 && oauthReqs.every((r) => oauthDone[r.key]);

  const autoCloseTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    if (onlyOauth && allOauthDone && !reconnecting) {
      setReconnecting(true);
      autoCloseTimerRef.current = setTimeout(() => {
        onComplete({}).catch(() => {});
      }, 1500);
    }
    return () => {
      if (autoCloseTimerRef.current !== null) {
        clearTimeout(autoCloseTimerRef.current);
      }
    };
    // We want this to fire when oauthDone changes — eslint exhaustive deps can be ignored here
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [allOauthDone, onlyOauth]);

  function setValue(key: string, val: string) {
    setValues((prev) => ({ ...prev, [key]: val }));
    setFieldErrors((prev) => { const n = { ...prev }; delete n[key]; return n; });
  }

  function validate(): boolean {
    const errors: Record<string, string> = {};
    for (const req of apiKeyReqs) {
      if (req.required && !values[req.key]?.trim() && !fulfilledMap[req.key]) {
        errors[req.key] = `${req.display_name} is required.`;
      }
    }
    for (const req of oauthReqs) {
      if (req.required && !oauthDone[req.key]) {
        errors[req.key] = `Please authorise ${req.display_name} before continuing.`;
      }
    }
    setFieldErrors(errors);
    return Object.keys(errors).length === 0;
  }

  async function handleSave() {
    if (!validate()) return;
    setSaving(true);
    setGlobalError(null);
    try {
      const secrets: Record<string, string> = {};
      for (const req of apiKeyReqs) {
        if (values[req.key]?.trim()) {
          secrets[req.key] = values[req.key].trim();
        }
      }
      await onComplete(secrets);
    } catch (err) {
      setGlobalError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  }

  function handleBackdropClick(e: React.MouseEvent<HTMLDivElement>) {
    if (e.target === e.currentTarget) onClose();
  }

  useEffect(() => {
    function handleKey(e: KeyboardEvent) {
      if (e.key === "Escape") onClose();
    }
    window.addEventListener("keydown", handleKey);
    return () => window.removeEventListener("keydown", handleKey);
  }, [onClose]);

  const hasMixedSecrets = apiKeyReqs.length > 0 && oauthReqs.length > 0;

  return (
    <div className="secret-modal-backdrop" onClick={handleBackdropClick}>
      <div className="secret-modal" role="dialog" aria-modal="true" aria-label={`Configure ${ext.name}`}>

        {/* Header */}
        <div className="secret-modal__header">
          <div className="secret-modal__header-left">
            <div className="secret-modal__icon">
              <KeyRound size={15} strokeWidth={1.8} />
            </div>
            <div className="secret-modal__title-group">
              <h2 className="secret-modal__title">
                {mode === "edit" ? `Update secrets — ${ext.name}` : `Configure ${ext.name}`}
              </h2>
              <p className="secret-modal__subtitle">
                {mode === "edit"
                  ? "Update credentials or re-authorise connections."
                  : "This extension needs credentials to work."}
              </p>
            </div>
          </div>
          <button
            type="button"
            className="secret-modal__close"
            onClick={onClose}
            aria-label="Close"
          >
            <X size={14} strokeWidth={2} />
          </button>
        </div>

        {/* Body */}
        <div className="secret-modal__body">

          {/* API key / generic fields */}
          {apiKeyReqs.length > 0 && (
            <>
              {hasMixedSecrets && (
                <p className="secret-modal__section-label">API credentials</p>
              )}
              {apiKeyReqs.map((req) => (
                <SecretField
                  key={req.key}
                  req={req}
                  value={values[req.key] ?? ""}
                  onChange={(v) => setValue(req.key, v)}
                  error={fieldErrors[req.key]}
                  fulfilled={fulfilledMap[req.key]}
                  disabled={saving}
                />
              ))}
            </>
          )}

          {/* Divider between mixed sections */}
          {hasMixedSecrets && <hr className="secret-modal__divider" />}

          {/* OAuth flow blocks */}
          {oauthReqs.length > 0 && (
            <>
              {hasMixedSecrets && (
                <p className="secret-modal__section-label">Account connections</p>
              )}
              {oauthReqs.map((req) => (
                <div key={req.key}>
                  <OAuthBlock
                    req={req}
                    extensionId={ext.id}
                    alreadyAuthorized={fulfilledMap[req.key] ?? false}
                    onAuthorized={() =>
                      setOauthDone((prev) => ({ ...prev, [req.key]: true }))
                    }
                    disabled={saving}
                  />
                  {fieldErrors[req.key] && (
                    <p className="secret-modal__field-error secret-modal__field-error--mt">
                      {fieldErrors[req.key]}
                    </p>
                  )}
                </div>
              ))}
            </>
          )}

          {/* Global error */}
          {globalError && (
            <div className="secret-modal__global-error">
              <AlertCircle size={13} strokeWidth={2} className="secret-modal__alert-icon" />
              {globalError}
            </div>
          )}
        </div>

        {/* Reconnecting overlay — shown when all oauth is done and we're auto-closing */}
        {reconnecting && (
          <div className="secret-modal__reconnect-banner">
            <div className="secret-modal__oauth-spinner" />
            <span>Reconnecting extension…</span>
          </div>
        )}

        {/* Footer actions — hidden when reconnecting in oauth-only mode */}
        {!reconnecting && (
          <div className="secret-modal__actions">
            <button
              type="button"
              className="secret-modal__cancel-btn"
              onClick={onClose}
              disabled={saving}
            >
              Cancel
            </button>
            <button
              type="button"
              className="secret-modal__save-btn"
              onClick={handleSave}
              disabled={saving}
            >
              {saving && <span className="secret-modal__save-spinner" />}
              {mode === "edit" ? "Save Changes" : "Save & Install"}
            </button>
          </div>
        )}
      </div>
    </div>
  );
}

// ── Extension Card ────────────────────────────────────────────

function ExtensionCard({
  ext,
  onToggle,
  onDelete,
  onConfigureSecrets,
  hasSecrets,
  secretStatus,
  disabled,
}: {
  ext: Extension;
  onToggle: (name: string, enabled: boolean) => void;
  onDelete: (name: string) => void;
  onConfigureSecrets?: (name: string) => void;
  hasSecrets?: boolean;
  secretStatus?: "configured" | "missing" | "unknown";
  disabled: boolean;
}) {
  const [expanded, setExpanded] = useState(false);

  return (
    <div className={`ext-card${ext.enabled ? " ext-card--enabled" : " ext-card--disabled"}`}>
      <div className="ext-card__header">
        {/* Icon + info */}
        <div className="ext-card__icon">
          <Puzzle size={15} strokeWidth={1.8} />
        </div>
        <div className="ext-card__info">
          <div className="ext-card__title-row">
            {ext.status && (
              <span
                className={`ext-card__status-dot ext-card__status-dot--${ext.status}`}
                title={
                  ext.status === "error" && ext.last_error
                    ? `Error: ${ext.last_error}`
                    : ext.status.charAt(0).toUpperCase() + ext.status.slice(1)
                }
                aria-label={`Status: ${ext.status}`}
              />
            )}
            <span className="ext-card__name">{ext.name}</span>
            <span className={`ext-card__kind-badge ext-card__kind-badge--${ext.kind === "stdio" ? "stdio" : "http"}`}>
              {ext.kind}
            </span>
            {ext.tools.length > 0 && (
              <span className="ext-card__tool-count">
                {ext.tools.length} {ext.tools.length === 1 ? "tool" : "tools"}
              </span>
            )}
            {/* Auth status badge — only shown for extensions that have required secrets */}
            {hasSecrets && secretStatus === "configured" && (
              <span className="ext-card__auth-badge ext-card__auth-badge--ok" title="All credentials configured">
                <Check size={9} strokeWidth={2.5} />
                Authorised
              </span>
            )}
            {hasSecrets && secretStatus === "missing" && (
              <button
                type="button"
                className="ext-card__auth-badge ext-card__auth-badge--warn"
                onClick={onConfigureSecrets ? () => onConfigureSecrets(ext.name) : undefined}
                title="Credentials required — click to configure"
              >
                <AlertCircle size={9} strokeWidth={2.5} />
                Setup required
              </button>
            )}
          </div>
          {ext.description && (
            <div className="ext-card__desc">{ext.description}</div>
          )}
          {ext.last_error && (
            <div className="ext-card__error-line" title={ext.last_error}>
              {ext.last_error}
            </div>
          )}
        </div>

        {/* Actions */}
        <div className="ext-card__actions">
          {hasSecrets && onConfigureSecrets && (
            <button
              className="ext-card__secrets-btn"
              onClick={() => onConfigureSecrets(ext.name)}
              aria-label={`Configure credentials for ${ext.name}`}
              title="Update credentials"
            >
              <KeyRound size={12} strokeWidth={1.8} />
            </button>
          )}
          {ext.tools.length > 0 && (
            <button
              className="ext-card__expand-btn"
              onClick={() => setExpanded((v) => !v)}
              aria-label={expanded ? "Collapse tools" : "Expand tools"}
              aria-expanded={expanded}
            >
              {expanded ? <ChevronUp size={14} strokeWidth={1.8} /> : <ChevronDown size={14} strokeWidth={1.8} />}
            </button>
          )}
          <Switch
            isSelected={ext.enabled}
            onChange={(val) => onToggle(ext.name, val)}
            isDisabled={disabled}
            size="sm"
            aria-label={`${ext.enabled ? "Disable" : "Enable"} ${ext.name}`}
          >
            <Switch.Content><Switch.Control><Switch.Thumb /></Switch.Control></Switch.Content>
          </Switch>
          <button
            className="ext-card__delete-btn"
            onClick={() => onDelete(ext.name)}
            disabled={disabled}
            aria-label={`Delete ${ext.name}`}
            title={`Delete ${ext.name}`}
          >
            <Trash2 size={13} strokeWidth={1.8} />
          </button>
        </div>
      </div>

      {/* Expandable tools list */}
      {expanded && ext.tools.length > 0 && (
        <div className="ext-card__tools">
          <div className="ext-card__tools-header">
            <Wrench size={11} strokeWidth={1.8} />
            <span>Available tools</span>
          </div>
          <div className="ext-card__tool-list">
            {ext.tools.map((tool) => (
              <div key={tool} className="ext-card__tool-item">
                <code>{tool}</code>
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

// ── Add Extension Form ────────────────────────────────────────

type ExtKind = "stdio" | "streamable_http";

function AddExtensionForm({
  onAdd,
  disabled,
}: {
  onAdd: (req: AddExtensionRequest) => Promise<void>;
  disabled: boolean;
}) {
  const [open, setOpen] = useState(false);
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const [name, setName] = useState("");
  const [kind, setKind] = useState<ExtKind>("stdio");
  const [command, setCommand] = useState("");
  const [args, setArgs] = useState("");
  const [uri, setUri] = useState("");

  function reset() {
    setName("");
    setKind("stdio");
    setCommand("");
    setArgs("");
    setUri("");
    setError(null);
  }

  async function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    if (!name.trim()) { setError("Name is required."); return; }
    if (kind === "stdio" && !command.trim()) { setError("Command is required for stdio extensions."); return; }
    if (kind === "streamable_http" && !uri.trim()) { setError("URI is required for HTTP extensions."); return; }

    const req: AddExtensionRequest = {
      name: name.trim(),
      kind,
      ...(kind === "stdio" && {
        command: command.trim(),
        args: args.trim() ? args.split(",").map((a) => a.trim()).filter(Boolean) : [],
      }),
      ...(kind === "streamable_http" && { uri: uri.trim() }),
    };

    setSubmitting(true);
    setError(null);
    try {
      await onAdd(req);
      reset();
      setOpen(false);
    } catch (err) {
      setError(String(err));
    } finally {
      setSubmitting(false);
    }
  }

  return (
    <div className="ext-add-accordion">
      <button
        className="ext-add-accordion__trigger"
        onClick={() => { setOpen((v) => !v); if (!open) setError(null); }}
        aria-expanded={open}
      >
        <span className="ext-add-accordion__trigger-label">
          <Plus size={13} strokeWidth={2} />
          Add Extension
        </span>
        {open ? <ChevronUp size={14} /> : <ChevronDown size={14} />}
      </button>

      {open && (
        <form className="ext-add-form" onSubmit={handleSubmit} noValidate>
          {/* Name */}
          <div className="ext-form-row">
            <label className="ext-form-label" htmlFor="ext-name">
              Name <span className="ext-form-required">*</span>
            </label>
            <input
              id="ext-name"
              className="ext-form-input"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="my-extension"
              required
              autoComplete="off"
              disabled={disabled || submitting}
            />
          </div>

          {/* Kind */}
          <div className="ext-form-row">
            <label className="ext-form-label" htmlFor="ext-kind">Kind</label>
            <select
              id="ext-kind"
              className="ext-form-select"
              value={kind}
              onChange={(e) => setKind(e.target.value as ExtKind)}
              disabled={disabled || submitting}
            >
              <option value="stdio">stdio</option>
              <option value="streamable_http">streamable_http</option>
            </select>
          </div>

          {/* Conditional: stdio */}
          {kind === "stdio" && (
            <>
              <div className="ext-form-row">
                <label className="ext-form-label" htmlFor="ext-command">
                  Command <span className="ext-form-required">*</span>
                </label>
                <input
                  id="ext-command"
                  className="ext-form-input"
                  value={command}
                  onChange={(e) => setCommand(e.target.value)}
                  placeholder="/usr/local/bin/my-mcp-server"
                  required
                  autoComplete="off"
                  disabled={disabled || submitting}
                />
              </div>
              <div className="ext-form-row">
                <label className="ext-form-label" htmlFor="ext-args">
                  Args
                  <span className="ext-form-hint"> (comma-separated)</span>
                </label>
                <input
                  id="ext-args"
                  className="ext-form-input"
                  value={args}
                  onChange={(e) => setArgs(e.target.value)}
                  placeholder="--port, 8080, --verbose"
                  autoComplete="off"
                  disabled={disabled || submitting}
                />
              </div>
            </>
          )}

          {/* Conditional: http */}
          {kind === "streamable_http" && (
            <div className="ext-form-row">
              <label className="ext-form-label" htmlFor="ext-uri">
                URI <span className="ext-form-required">*</span>
              </label>
              <input
                id="ext-uri"
                className="ext-form-input"
                value={uri}
                onChange={(e) => setUri(e.target.value)}
                placeholder="http://localhost:3001/mcp"
                type="url"
                required
                autoComplete="off"
                disabled={disabled || submitting}
              />
            </div>
          )}

          {/* Error */}
          {error && (
            <p className="ext-form-error">{error}</p>
          )}

          {/* Actions */}
          <div className="ext-form-actions">
            <Button
              variant="ghost"
              size="sm"
              onPress={() => { setOpen(false); reset(); }}
              isDisabled={submitting}
              type="button"
            >
              Cancel
            </Button>
            <Button
              variant="secondary"
              size="sm"
              type="submit"
              isDisabled={disabled || submitting}
            >
              {submitting ? "Adding…" : "Add Extension"}
            </Button>
          </div>
        </form>
      )}
    </div>
  );
}

// ── Marketplace Card ──────────────────────────────────────────

function MarketplaceCard({
  ext,
  isInstalled,
  onInstall,
}: {
  ext: MarketplaceExtension;
  isInstalled: boolean;
  onInstall: (id: string, secrets?: Record<string, string>) => Promise<void>;
}) {
  const [installing, setInstalling] = useState(false);
  const [justInstalled, setJustInstalled] = useState(false);
  const [installError, setInstallError] = useState<string | null>(null);
  const [showSecretModal, setShowSecretModal] = useState(false);

  const needsSecrets = ext.required_secrets && ext.required_secrets.length > 0;

  async function handleInstallClick() {
    if (isInstalled || justInstalled || installing) return;
    if (needsSecrets) {
      setShowSecretModal(true);
    } else {
      await doInstall();
    }
  }

  async function doInstall(secrets?: Record<string, string>) {
    setInstalling(true);
    setInstallError(null);
    try {
      await onInstall(ext.id, secrets);
      setJustInstalled(true);
      setShowSecretModal(false);
    } catch (err) {
      setInstallError(err instanceof Error ? err.message : String(err));
      throw err; // let modal show the error inline
    } finally {
      setInstalling(false);
    }
  }

  const installed = isInstalled || justInstalled;

  return (
    <>
      <div className={`mkt-card${ext.featured ? " mkt-card--featured" : ""}`}>
        {ext.featured && (
          <div className="mkt-card__featured-badge">
            <Star size={9} strokeWidth={2} />
            Featured
          </div>
        )}
        <div className="mkt-card__header">
          <div className="mkt-card__icon">
            <Puzzle size={15} strokeWidth={1.8} />
          </div>
          <div className="mkt-card__info">
            <div className="mkt-card__name">{ext.name}</div>
            <div className="mkt-card__meta">
              <span className="mkt-card__category-badge">{ext.category}</span>
              <span className="mkt-card__tool-count">
                {ext.tools.length} {ext.tools.length === 1 ? "tool" : "tools"}
              </span>
              {needsSecrets && (
                <span className="mkt-card__tool-count" title="Requires credentials">
                  <KeyRound size={9} strokeWidth={2} className="icon-inline" /> credentials
                </span>
              )}
            </div>
          </div>
          <div className="mkt-card__action">
            {installed ? (
              <span className="mkt-card__installed-badge">
                <Check size={11} strokeWidth={2.5} />
                Installed
              </span>
            ) : (
              <button
                className="mkt-card__install-btn"
                onClick={handleInstallClick}
                disabled={installing}
                aria-label={`Install ${ext.name}`}
              >
                {installing ? (
                  <span className="mkt-card__install-spinner" />
                ) : (
                  <Download size={12} strokeWidth={2} />
                )}
                {installing ? "Installing…" : "Install"}
              </button>
            )}
          </div>
        </div>
        <p className="mkt-card__desc">{ext.description}</p>
        {installError && (
          <div className="ext-card__error-line" title={installError}>
            {installError}
          </div>
        )}
        <div className="mkt-card__footer">
          <span className="mkt-card__author">by {ext.author}</span>
          <span className={`mkt-card__kind-badge mkt-card__kind-badge--${ext.kind === "stdio" ? "stdio" : "http"}`}>
            {ext.kind}
          </span>
        </div>
      </div>

      {showSecretModal && (
        <SecretConfigModal
          ext={ext}
          mode="install"
          onClose={() => setShowSecretModal(false)}
          onComplete={doInstall}
        />
      )}
    </>
  );
}

// ── Browse Tab ────────────────────────────────────────────────

function BrowseTab({
  installedExtensions,
  onInstallSuccess,
}: {
  installedExtensions: Extension[];
  onInstallSuccess: (ext: Extension) => void;
}) {
  const [items, setItems] = useState<MarketplaceExtension[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    api.listMarketplace()
      .then((list) => { if (!cancelled) setItems(list); })
      .catch((err) => { if (!cancelled) setError(String(err)); })
      .finally(() => { if (!cancelled) setLoading(false); });
    return () => { cancelled = true; };
  }, []);

  const installedNames = new Set(installedExtensions.map((e) => e.name.toLowerCase()));

  async function handleInstall(id: string, secrets?: Record<string, string>) {
    const ext = await api.installMarketplaceExtension(id, secrets);
    onInstallSuccess(ext);
  }

  if (loading) {
    return (
      <div className="mkt-loading">
        <div className="mkt-loading__spinner" />
        <span>Loading marketplace…</span>
      </div>
    );
  }

  if (error) {
    return (
      <div className="mkt-error">
        <AlertCircle size={16} strokeWidth={1.8} />
        <span>{error}</span>
      </div>
    );
  }

  if (items.length === 0) {
    return (
      <div className="empty-state">
        <Store size={32} strokeWidth={1.2} />
        <div>
          <div className="empty-state__heading">No extensions available</div>
          <div className="empty-state__body">The marketplace is empty right now. Check back later.</div>
        </div>
      </div>
    );
  }

  const sorted = [...items].sort((a, b) => {
    if (a.featured && !b.featured) return -1;
    if (!a.featured && b.featured) return 1;
    return a.name.localeCompare(b.name);
  });

  return (
    <div className="mkt-grid">
      {sorted.map((ext) => (
        <MarketplaceCard
          key={ext.id}
          ext={ext}
          isInstalled={installedNames.has(ext.id) || installedNames.has(ext.name.toLowerCase())}
          onInstall={handleInstall}
        />
      ))}
    </div>
  );
}

// ── Tools Tab ────────────────────────────────────────────────
// Every enabled extension's tools with descriptions; Installed cards show bare names only.

function ToolsTab() {
  const [tools, setTools]     = useState<AgentTool[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError]     = useState<string | null>(null);

  const load = useCallback(() => {
    setError(null);
    setLoading(true);
    api.listTools().then(setTools).catch((e) => setError(String(e))).finally(() => setLoading(false));
  }, []);

  useEffect(() => { load(); }, [load]);

  if (loading) return <p className="ext-status-text">Loading tools…</p>;
  if (error)   return <ErrorBanner error={error} onRetry={load} />;
  if (!tools.length) return (
    <div className="empty-state">
      <Wrench size={32} strokeWidth={1.2} />
      <div>
        <div className="empty-state__heading">No MCP tools loaded</div>
        <div className="empty-state__body">Enable an extension to see the tools it provides.</div>
      </div>
    </div>
  );

  const byExtension: Record<string, AgentTool[]> = {};
  for (const t of tools) {
    (byExtension[t.extension] ??= []).push(t);
  }

  return (
    <div className="ext-list-stack">
      {Object.entries(byExtension).map(([ext, extTools]) => (
        <Card key={ext} className="card">
          <CardContent className="card-body--flush">
            <div className="agent-ext-header">
              <span className="ext-card__name">{ext}</span>
              <span className="muted-12 agent-ext-count">
                {extTools.length} tool{extTools.length !== 1 ? "s" : ""}
              </span>
            </div>
            <div>
              {extTools.map((t) => (
                <div key={t.name} className="tool-row">
                  <span className="tool-row__icon"><Terminal size={14} /></span>
                  <span className="tool-row__name">{t.name}</span>
                  {t.description && (
                    <span className="ext-card__tool-count tool-desc-chip">
                      {t.description.length > 80 ? t.description.slice(0, 77) + "..." : t.description}
                    </span>
                  )}
                  <span className="tool-row__spacer" />
                </div>
              ))}
            </div>
          </CardContent>
        </Card>
      ))}
    </div>
  );
}

// ── Main Extensions Component ─────────────────────────────────

type Tab = "installed" | "tools" | "browse";

interface SecretEditState {
  extName: string;
  /** Marketplace entry, for its required_secrets list. */
  mktExt: MarketplaceExtension | null;
  fulfilledMap: Record<string, boolean>;
}

export function Extensions() {
  const state = useAppState();
  const confirm = useConfirm();
  const [activeTab, setActiveTab] = useState<Tab>("installed");
  const [extensions, setExtensions] = useState<Extension[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [actionMsg, setActionMsg] = useState<{ text: string; ok: boolean } | null>(null);

  // For edit-mode secret modal on installed extensions
  const [marketplaceCache, setMarketplaceCache] = useState<MarketplaceExtension[]>([]);
  const [secretEditState, setSecretEditState] = useState<SecretEditState | null>(null);
  const [secretStatus, setSecretStatus] = useState<Record<string, "configured" | "missing" | "unknown">>({});

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const res = await api.listExtensions();
      const list = Array.isArray(res)
        ? (res as Extension[])
        : ((res as { extensions: Extension[] }).extensions ?? []);
      setExtensions(list);
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => { load(); }, [load]);

  function flash(text: string, ok = true) {
    setActionMsg({ text, ok });
    setTimeout(() => setActionMsg(null), 3500);
  }

  async function handleToggle(name: string, enabled: boolean) {
    try {
      await api.toggleExtension(name, enabled);
      setExtensions((prev) =>
        prev.map((e) => e.name === name ? { ...e, enabled } : e),
      );
      flash(`${name} ${enabled ? "enabled" : "disabled"}.`);
    } catch (err) {
      flash(String(err), false);
    }
  }

  async function handleDelete(name: string) {
    if (!await confirm(`Delete extension "${name}"? This cannot be undone.`, { title: "Delete Extension", confirmLabel: "Delete", destructive: true })) return;
    try {
      await api.removeExtension(name);
      setExtensions((prev) => prev.filter((e) => e.name !== name));
      flash(`${name} removed.`);
    } catch (err) {
      flash(String(err), false);
    }
  }

  async function handleAdd(req: AddExtensionRequest) {
    const ext = await api.addExtension(req);
    setExtensions((prev) => [...prev, ext]);
    flash(`${ext.name} added.`);
  }

  // Lazily load marketplace listing to get required_secrets for installed exts
  useEffect(() => {
    if (marketplaceCache.length === 0) {
      api.listMarketplace()
        .then((list) => setMarketplaceCache(list))
        .catch(() => { /* non-critical, ignore */ });
    }
  }, [marketplaceCache.length]);

  useEffect(() => {
    if (marketplaceCache.length === 0 || extensions.length === 0) return;

    const extsWithSecrets = extensions.filter((ext) => {
      const mktEntry = marketplaceCache.find(
        (m) => m.name.toLowerCase() === ext.name.toLowerCase() || m.id.toLowerCase() === ext.name.toLowerCase(),
      );
      return (mktEntry?.required_secrets?.length ?? 0) > 0;
    });

    if (extsWithSecrets.length === 0) return;

    for (const ext of extsWithSecrets) {
      api.getExtensionSecrets(ext.name)
        .then((res) => {
          const allFulfilled = Object.values(res.fulfilled).length > 0 && Object.values(res.fulfilled).every(Boolean);
          setSecretStatus((prev) => ({
            ...prev,
            [ext.name]: allFulfilled ? "configured" : "missing",
          }));
        })
        .catch(() => {
          setSecretStatus((prev) => ({ ...prev, [ext.name]: "unknown" }));
        });
    }
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [extensions.length, marketplaceCache.length]);

  async function handleConfigureSecrets(extName: string) {
    const mktExt = marketplaceCache.find(
      (m) => m.name.toLowerCase() === extName.toLowerCase() || m.id.toLowerCase() === extName.toLowerCase(),
    ) ?? null;

    if (!mktExt || mktExt.required_secrets.length === 0) return;

    let fulfilledMap: Record<string, boolean> = {};
    try {
      const res = await api.getExtensionSecrets(extName);
      fulfilledMap = res.fulfilled;
    } catch {
      // ignore — we'll show unfilled state
    }

    setSecretEditState({ extName, mktExt, fulfilledMap });
  }

  /** Refresh one extension's auth badge. Call it on every `handleSecretEditComplete` path that
   *  stored or completed something, including OAuth-only completion (payload `{}`). */
  function refreshSecretBadge(extName: string) {
    api.getExtensionSecrets(extName)
      .then((res) => {
        const allFulfilled = Object.values(res.fulfilled).length > 0 && Object.values(res.fulfilled).every(Boolean);
        setSecretStatus((prev) => ({
          ...prev,
          [extName]: allFulfilled ? "configured" : "missing",
        }));
      })
      .catch(() => {});
  }

  async function handleSecretEditComplete(secrets: Record<string, string>) {
    if (!secretEditState) return;
    const extName = secretEditState.extName;
    setSecretEditState(null);

    if (Object.keys(secrets).length === 0) {
      flash(`Credentials updated for ${extName}.`);
      refreshSecretBadge(extName);
      return;
    }

    let result;
    try {
      result = await api.setExtensionSecrets(extName, secrets);
    } catch (err) {
      // Nothing was stored, so there is no new state to reflect anywhere.
      flash(`Could not save credentials for ${extName}: ${String(err)}`, false);
      return;
    }

    // `request()` yields undefined for a 204 or non-JSON 2xx, e.g. from an older sidecar.
    const outcome = result ?? { stored: 0, restarted: false, restart_error: null };

    if (outcome.restart_error) {
      // Stored but not running; `load()` puts the lasting error status on the card.
      flash(
        `Credentials saved, but ${extName} did not restart: ${outcome.restart_error}`,
        false,
      );
      load();
    } else if (outcome.restarted) {
      flash(`Credentials updated for ${extName}. Restarted to apply them.`);
      load();
    } else {
      flash(`Credentials updated for ${extName}.`);
    }

    refreshSecretBadge(extName);
  }

  function handleInstallFromMarketplace(ext: Extension) {
    setExtensions((prev) => {
      const exists = prev.some((e) => e.name === ext.name);
      return exists ? prev : [...prev, ext];
    });
    flash(`${ext.name} installed.`);
    setActiveTab("installed");
  }

  const enabledCount = extensions.filter((e) => e.enabled).length;

  return (
    <div className="screen">
      {/* Page header */}
      <div className="page-header">
        <h1 className="page-header__title">Extensions</h1>
        <div className="page-header__action">
          {activeTab === "installed" && (
            <Button size="sm" variant="ghost" onPress={load} isDisabled={loading}>
              <RefreshCw size={14} strokeWidth={1.8} style={{ opacity: loading ? 0.4 : 1 }} />
              Refresh
            </Button>
          )}
        </div>
      </div>

      {/* Summary banner */}
      <div className="seg-banner seg-banner--info">
        <Puzzle size={14} />
        <span>
          MCP server extensions give the agent additional tools.
          {!loading && (
            <> {extensions.length} registered, {enabledCount} active.</>
          )}
        </span>
      </div>

      {/* Tab bar */}
      <div className="ext-tab-bar" role="tablist">
        <button
          className={`ext-tab-bar__tab${activeTab === "installed" ? " ext-tab-bar__tab--active" : ""}`}
          role="tab"
          aria-selected={activeTab === "installed"}
          onClick={() => setActiveTab("installed")}
        >
          Installed
          {extensions.length > 0 && (
            <span className="ext-tab-bar__count">{extensions.length}</span>
          )}
        </button>
        <button
          className={`ext-tab-bar__tab${activeTab === "tools" ? " ext-tab-bar__tab--active" : ""}`}
          role="tab"
          aria-selected={activeTab === "tools"}
          onClick={() => setActiveTab("tools")}
        >
          <Wrench size={12} strokeWidth={2} />
          Tools
        </button>
        <button
          className={`ext-tab-bar__tab${activeTab === "browse" ? " ext-tab-bar__tab--active" : ""}`}
          role="tab"
          aria-selected={activeTab === "browse"}
          onClick={() => setActiveTab("browse")}
        >
          <Store size={12} strokeWidth={2} />
          Browse
        </button>
      </div>

      {/* Action feedback */}
      {actionMsg && (
        <p
          className="ext-action-msg"
          style={{ color: actionMsg.ok ? "var(--color-success)" : "var(--color-destructive)" }}
        >
          {actionMsg.text}
        </p>
      )}

      {/* Tab panels */}
      {activeTab === "installed" && (
        <>
          {/* Extensions list */}
          <Card className="card">
            <CardContent>
              {loading && (
                <p className="ext-status-text">Loading extensions…</p>
              )}
              {!loading && error && (
                <p className="ext-status-text ext-status-text--error">{error}</p>
              )}
              {!loading && !error && extensions.length === 0 && (
                <div className="empty-state">
                  <Puzzle size={32} strokeWidth={1.2} />
                  <div>
                    <div className="empty-state__heading">No extensions registered</div>
                    <div className="empty-state__body">
                      Add an MCP server extension below, or{" "}
                      <button
                        className="ext-inline-link"
                        onClick={() => setActiveTab("browse")}
                      >
                        browse the marketplace
                      </button>
                      .
                    </div>
                  </div>
                </div>
              )}
              {!loading && !error && extensions.length > 0 && (
                <div className="ext-list-stack">
                  {extensions.map((ext) => {
                    const mktEntry = marketplaceCache.find(
                      (m) => m.name.toLowerCase() === ext.name.toLowerCase() || m.id.toLowerCase() === ext.name.toLowerCase(),
                    );
                    const hasSecrets = (mktEntry?.required_secrets?.length ?? 0) > 0;
                    return (
                      <ExtensionCard
                        key={ext.name}
                        ext={ext}
                        onToggle={handleToggle}
                        onDelete={handleDelete}
                        onConfigureSecrets={hasSecrets ? handleConfigureSecrets : undefined}
                        hasSecrets={hasSecrets}
                        secretStatus={hasSecrets ? (secretStatus[ext.name] ?? "unknown") : undefined}
                        disabled={!state.serverOnline}
                      />
                    );
                  })}
                </div>
              )}
            </CardContent>
          </Card>

          {/* Add form */}
          <AddExtensionForm onAdd={handleAdd} disabled={!state.serverOnline} />
        </>
      )}

      {activeTab === "tools" && <ToolsTab />}

      {activeTab === "browse" && (
        <Card className="card">
          <CardContent>
            <BrowseTab
              installedExtensions={extensions}
              onInstallSuccess={handleInstallFromMarketplace}
            />
          </CardContent>
        </Card>
      )}

      {/* Edit-mode secret config modal for installed extensions */}
      {secretEditState && secretEditState.mktExt && (
        <SecretConfigModal
          ext={secretEditState.mktExt}
          mode="edit"
          fulfilledMap={secretEditState.fulfilledMap}
          onClose={() => setSecretEditState(null)}
          onComplete={handleSecretEditComplete}
        />
      )}
    </div>
  );
}
