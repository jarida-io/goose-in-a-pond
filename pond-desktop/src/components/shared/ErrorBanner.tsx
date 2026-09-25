import { AlertTriangle, RefreshCw } from "lucide-react";

/** Maps a raw error string to a short user-facing message, falling back to a generic one. */
export function friendlyMessage(raw: string): string {
  const s = raw.toLowerCase();
  if (s.includes("network") || s.includes("fetch") || s.includes("failed to fetch"))
    return "Could not reach the server. Check that it is running and try again.";
  if (s.includes("401") || s.includes("unauthorized"))
    return "Your session has expired. Reload the page to sign in again.";
  if (s.includes("403") || s.includes("forbidden"))
    return "You don't have permission to do that.";
  if (s.includes("404") || s.includes("not found"))
    return "The requested resource was not found.";
  if (s.includes("500") || s.includes("internal server"))
    return "The server encountered an error. Try again in a moment.";
  if (s.includes("timeout") || s.includes("timed out"))
    return "The request timed out. The server may be busy — try again.";
  if (s.includes("econnrefused") || s.includes("connection refused"))
    return "Could not connect to the server. Make sure it is running.";
  return "Something went wrong. Please try again.";
}

interface ErrorBannerProps {
  error: string;
  onRetry?: () => void;
  retryLabel?: string;
}

export function ErrorBanner({ error, onRetry, retryLabel = "Try again" }: ErrorBannerProps) {
  return (
    <div className="error-banner" role="alert">
      <AlertTriangle size={15} className="error-banner__icon" />
      <span className="error-banner__msg">{friendlyMessage(error)}</span>
      {onRetry && (
        <button className="error-banner__retry" onClick={onRetry}>
          <RefreshCw size={12} />
          {retryLabel}
        </button>
      )}
    </div>
  );
}
