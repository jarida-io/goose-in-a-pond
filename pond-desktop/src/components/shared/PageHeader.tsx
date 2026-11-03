import type { ReactNode } from "react";

export function PageHeader({ title, subtitle, action }: { title: string; subtitle?: string; action?: ReactNode }) {
  const heading = <h1 className="page-header__title">{title}</h1>;
  return (
    <div className="page-header">
      {subtitle ? <div>{heading}<p className="page-header__sub">{subtitle}</p></div> : heading}
      {action && <div className="page-header__action">{action}</div>}
    </div>
  );
}
