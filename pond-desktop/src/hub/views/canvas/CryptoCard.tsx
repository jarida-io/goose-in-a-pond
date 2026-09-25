import React from "react";
import { HubIco } from "../../primitives/HubIco";
import { Sparkline } from "./Sparkline";

// ── Mock data ──────────────────────────────────────────────────

interface CoinRow {
  sym: string;
  name: string;
  price: string;
  chg: number;
  spark: number[];
}

const COINS: CoinRow[] = [
  { sym: "BTC", name: "Bitcoin",  price: "$67,842", chg:  2.34, spark: [64, 65, 63, 66, 67, 68, 67] },
  { sym: "ETH", name: "Ethereum", price: "$3,456",  chg: -1.12, spark: [35, 36, 35, 34, 34, 35, 34] },
  { sym: "SOL", name: "Solana",   price: "$172.50", chg:  5.67, spark: [155, 160, 158, 165, 168, 170, 172] },
  { sym: "ADA", name: "Cardano",  price: "$0.48",   chg: -0.85, spark: [50, 49, 50, 49, 48, 49, 48] },
];

const CHART_PATH  = "M22 12h-4l-3 9L9 3l-3 9H2";
const UP_ARROW    = "M23 6l-9.5 9.5-5-5L1 18";
const DOWN_ARROW  = "M23 18l-9.5-9.5-5 5L1 6";

/** Crypto card for giap-finance.get_crypto_price; mock data for now. */
export function CryptoCard(): React.ReactElement {
  return (
    <div className="mc">
      <div className="mc-header">
        <div className="mc-header-left">
          <HubIco d={CHART_PATH} size={15} color="#10B981" sw={1.75} />
          <span className="mc-title">Crypto prices</span>
        </div>
        <span className="mc-chip mc-chip--green mc-chip--live">
          <span className="mc-live-dot" aria-hidden="true" />
          Live
        </span>
      </div>
      <div className="mc-divider" />
      <div style={{ flex: 1, display: "flex", flexDirection: "column", padding: "8px 0" }}>
        {COINS.map((c, i) => {
          const up = c.chg >= 0;
          return (
            <div
              key={c.sym}
              style={{
                display: "flex",
                alignItems: "center",
                padding: "10px 16px",
                borderBottom: i < COINS.length - 1 ? "1px solid #F8FAFC" : "none",
                gap: 10,
              }}
            >
              {/* Coin avatar */}
              <div
                style={{
                  width: 36,
                  height: 36,
                  borderRadius: 10,
                  background: up ? "#F0FDF4" : "#FFF1F2",
                  display: "flex",
                  alignItems: "center",
                  justifyContent: "center",
                  flexShrink: 0,
                  border: `1px solid ${up ? "#DCFCE7" : "#FFE4E6"}`,
                }}
              >
                <span
                  style={{
                    fontSize: 11,
                    fontWeight: 800,
                    color: up ? "var(--color-success-fg)" : "var(--color-destructive-fg)",
                  }}
                >
                  {c.sym}
                </span>
              </div>

              {/* Name */}
              <div style={{ flex: 1, minWidth: 0 }}>
                <div style={{ fontSize: 13, fontWeight: 700, color: "#18181B" }}>{c.sym}</div>
                <div style={{ fontSize: 11, color: "var(--color-text-tertiary)", fontWeight: 500 }}>{c.name}</div>
              </div>

              {/* Sparkline */}
              <Sparkline data={c.spark} positive={up} />

              {/* Price + change */}
              <div style={{ textAlign: "right", flexShrink: 0 }}>
                <div style={{ fontSize: 13, fontWeight: 800, color: "#18181B" }}>{c.price}</div>
                <div
                  style={{
                    fontSize: 11,
                    fontWeight: 700,
                    color: up ? "var(--color-success-fg)" : "var(--color-destructive-fg)",
                    display: "flex",
                    alignItems: "center",
                    justifyContent: "flex-end",
                    gap: 3,
                    marginTop: 2,
                  }}
                >
                  <HubIco d={up ? UP_ARROW : DOWN_ARROW} size={11} color={up ? "var(--color-success-fg)" : "var(--color-destructive-fg)"} sw={2} />
                  {up ? "+" : ""}{c.chg.toFixed(2)}%
                </div>
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}
