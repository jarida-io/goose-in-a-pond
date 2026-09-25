import { ChevronLeft } from "lucide-react";

// ─── DetailShell ──────────────────────────────────────────────
// Shared wrapper for every settings sub-screen: back button, title row, scrollable body.

interface DetailShellProps {
  title: string;
  subtitle: string;
  accent?: string;
  headRight?: React.ReactNode;
  onBack: () => void;
  children: React.ReactNode;
}

export function DetailShell({
  title,
  subtitle,
  accent = "var(--pp)",
  headRight,
  onBack,
  children,
}: DetailShellProps) {
  return (
    <div className="setd">
      <header className="setd__head">
        <button
          className="setd__back"
          type="button"
          onClick={onBack}
          aria-label="Back to Settings"
        >
          <ChevronLeft size={17} strokeWidth={2.5} />
          Settings
        </button>
        <div className="setd__titlerow">
          <div>
            <h1 className="view-title" style={{ color: accent }}>
              {title}
            </h1>
            <p className="view-sub">{subtitle}</p>
          </div>
          {headRight}
        </div>
      </header>
      <div className="setd__body">{children}</div>
    </div>
  );
}
