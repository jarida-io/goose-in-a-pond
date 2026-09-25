// ─── One time-zone picker, everywhere ────────────────────────────────────────
// Lists the server's IANA catalogue, with offsets so two similar names can be told apart.

import { useEffect, useState } from "react";
import { allZones, deviceZone } from "../lib/place";
import type { ZoneChoice } from "../api/types";

interface Props {
  value: string;
  onChange: (zone: string) => void;
  className?: string;
  "aria-label"?: string;
  id?: string;
}

export function ZonePicker({ value, onChange, className, id, ...rest }: Props) {
  const [zones, setZones] = useState<ZoneChoice[] | null>(null);

  useEffect(() => {
    let cancelled = false;
    // `allZones` never rejects (it falls back to the webview's catalogue), so no error branch.
    allZones().then((z) => {
      if (!cancelled) setZones(z);
    });
    return () => {
      cancelled = true;
    };
  }, []);

  const current = value || deviceZone();
  const list = zones ?? [];
  // Keep and show a stored zone the catalogue lacks: opening the picker must never change it.
  const missing = current && !list.some((z) => z.zone === current);

  return (
    <select
      id={id}
      className={className}
      value={current}
      onChange={(e) => onChange(e.target.value)}
      aria-label={rest["aria-label"]}
    >
      {missing && <option value={current}>{current}</option>}
      {list.map((z) => (
        <option key={z.zone} value={z.zone}>
          {z.place ? `${z.zone} — ${z.place} (${z.offset})` : `${z.zone} (${z.offset})`}
        </option>
      ))}
    </select>
  );
}
