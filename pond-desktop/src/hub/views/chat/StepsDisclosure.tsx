import { useState } from "react";
import { ChevronRight } from "lucide-react";
import i18n from "../../../i18n";

/** What the model wrote while it worked, kept out of the answer: one faint line that opens. */
export function StepsDisclosure({ steps }: { steps: string[] }) {
  const [open, setOpen] = useState(false);

  return (
    <div className={`think think--steps${open ? " is-open" : ""}`}>
      <button
        type="button"
        className="think__toggle"
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open}
      >
        <span className="think__label">{i18n.t("chat.steps")}</span>
        <ChevronRight className="think__chev" size={12} aria-hidden="true" />
      </button>

      {open && (
        <div className="think__body">
          {steps.map((step, i) => (
            <p key={i}>{step}</p>
          ))}
        </div>
      )}
    </div>
  );
}
