import React, { useState } from "react";
import ReactDOM from "react-dom/client";

import "./styles/design-tokens.css";
import "./styles/base.css";
import "./styles/sections.css";

import { StartupScreen } from "./components/StartupScreen";
import { AppContextProvider } from "./state/AppContext";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { App } from "./App";

function Root() {
  const [ready, setReady] = useState(false);

  if (!ready) {
    return <StartupScreen onReady={() => setReady(true)} />;
  }

  // ErrorBoundary wraps the entire app so any render-time exception in a
  // section (Voice mode, Chat, etc.) shows a readable error card with a
  // "Try again" button instead of leaving the user with a blank window
  // and no way out except force-quitting.
  return (
    <ErrorBoundary>
      <AppContextProvider>
        <App />
      </AppContextProvider>
    </ErrorBoundary>
  );
}

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <Root />
  </React.StrictMode>,
);
