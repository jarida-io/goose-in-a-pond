import { useId, type ReactNode } from "react";
import { AlertCircle } from "lucide-react";

/** What a control needs so its hint and the pond's answer are read with it. */
export interface ControlProps {
  id: string;
  "aria-describedby"?: string;
  "aria-invalid"?: true;
}

export function Messages({ id, messages }: { id?: string; messages: readonly string[] }) {
  if (messages.length === 0) return null;
  return (
    <ul className="ag-errors" id={id}>
      {messages.map((message, i) => (
        <li key={i} className="ag-errors__item">
          <AlertCircle size={14} aria-hidden="true" />
          <span>{message}</span>
        </li>
      ))}
    </ul>
  );
}

interface FieldProps {
  label: string;
  hint?: ReactNode;
  errors?: readonly string[];
  className?: string;
  children: (control: ControlProps) => ReactNode;
}

/** A labelled control with its hint, and the pond's messages for it directly beneath. */
export function Field({ label, hint, errors = [], className, children }: FieldProps) {
  const id = useId();
  const hintId = `${id}-hint`;
  const errorId = `${id}-errors`;
  const described = [hint ? hintId : "", errors.length > 0 ? errorId : ""].filter(Boolean).join(" ");
  return (
    <div className={`ag-field${className ? ` ${className}` : ""}`}>
      <label className="ag-label" htmlFor={id}>
        {label}
      </label>
      {children({
        id,
        "aria-describedby": described || undefined,
        "aria-invalid": errors.length > 0 ? true : undefined,
      })}
      <Messages id={errorId} messages={errors} />
      {hint ? (
        <p className="ag-hint" id={hintId}>
          {hint}
        </p>
      ) : null}
    </div>
  );
}

interface FormSectionProps {
  id: string;
  title: string;
  lead?: ReactNode;
  problems?: number;
  children: ReactNode;
}

export function FormSection({ id, title, lead, problems = 0, children }: FormSectionProps) {
  const headingId = `${id}-heading`;
  return (
    <section className="ag-sec" id={id} aria-labelledby={headingId}>
      <header className="ag-sec__head">
        <h2 className="ag-sec__title" id={headingId}>
          {title}
        </h2>
        {problems > 0 ? (
          <span className="ag-sec__count">{problems === 1 ? "1 problem" : `${problems} problems`}</span>
        ) : null}
      </header>
      {lead ? <p className="ag-sec__lead">{lead}</p> : null}
      <div className="ag-sec__body">{children}</div>
    </section>
  );
}
