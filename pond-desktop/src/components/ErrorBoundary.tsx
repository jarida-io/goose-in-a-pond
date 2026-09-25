import React from "react";

// Shows a readable error screen with "Try again" instead of a blank window on a render error.

interface Props {
  children: React.ReactNode;
}

interface State {
  err: Error | null;
}

export class ErrorBoundary extends React.Component<Props, State> {
  state: State = { err: null };

  static getDerivedStateFromError(err: Error): State {
    return { err };
  }

  componentDidCatch(err: Error, info: React.ErrorInfo) {
    // Logged so DevTools shows the stack the friendly screen hides.
    // eslint-disable-next-line no-console
    console.error("[ErrorBoundary] React render failed:", err, info?.componentStack);
  }

  reset = () => this.setState({ err: null });

  render() {
    if (!this.state.err) return this.props.children;

    const message = this.state.err.message || String(this.state.err);
    return (
      <div style={styles.root} role="alert">
        <div style={styles.card}>
          <h2 style={styles.title}>Something went wrong</h2>
          <p style={styles.body}>
            Pond's interface hit an error and stopped rendering. The server is
            still running, so your data is safe — try the screen again, or
            switch to a different section.
          </p>
          <pre style={styles.detail}>{message}</pre>
          <button style={styles.btn} onClick={this.reset}>Try again</button>
        </div>
      </div>
    );
  }
}

const styles: Record<string, React.CSSProperties> = {
  root: {
    minHeight: "100vh",
    width: "100%",
    display: "flex",
    alignItems: "center",
    justifyContent: "center",
    padding: "24px",
    background: "var(--color-bg, #f7f5f2)",
  },
  card: {
    maxWidth: 520,
    background: "#fff",
    border: "1px solid rgba(0,0,0,0.08)",
    borderRadius: 12,
    padding: "20px 22px",
    boxShadow: "0 6px 24px rgba(0,0,0,0.08)",
  },
  title: {
    margin: "0 0 8px",
    fontSize: 18,
    fontFamily: "var(--font-display, system-ui)",
    color: "#1f2937",
  },
  body: {
    margin: "0 0 12px",
    fontSize: 14,
    color: "#4b5563",
    lineHeight: 1.5,
  },
  detail: {
    margin: "0 0 16px",
    padding: "10px 12px",
    background: "#f3f4f6",
    border: "1px solid #e5e7eb",
    borderRadius: 8,
    fontFamily: "var(--font-mono, ui-monospace)",
    fontSize: 12,
    color: "#7f1d1d",
    whiteSpace: "pre-wrap",
    overflowWrap: "anywhere",
    maxHeight: 220,
    overflowY: "auto",
  },
  btn: {
    background: "#8C52FF",
    color: "#ffffff",
    border: "none",
    borderRadius: 8,
    padding: "8px 16px",
    fontSize: 13,
    fontWeight: 600,
    cursor: "pointer",
  },
};
