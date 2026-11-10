// Must stay first: applies the stored theme to <html> before React renders (no flash).
import "./hub/state/themeBootstrap";
import { captureSignInLink, followSignInLinks } from "./api/hostCredential";

import React from "react";
import ReactDOM from "react-dom/client";

import "./styles/design-tokens.css";
// Ink styles are structure only; every value reads a property `themeStore` set on <html>.
import "@jarida/ink/css";
import "./styles/base.css";
import "./styles/sections.css";
import "./hub/hub.css";
// Last: the touch layer settles equal-specificity conflicts by cascade order.
import "./styles/touch.css";

import { Root } from "./Root";
// One-time move of member preferences from localStorage to the server; never throws or blocks.
import { migrateLocalProfileToServer } from "./api/migrateLocalProfile";

// Before anything renders or logs the URL: moves a `#host=` credential out of the address bar.
try {
  captureSignInLink(window.location, window.history, window.sessionStorage);
} catch (e) {
  console.warn("[host] session storage is unavailable; this tab cannot be signed in", e);
}
followSignInLinks(window, () => window.location.reload());

void migrateLocalProfileToServer();

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <Root />
  </React.StrictMode>,
);
