import { createTheme, cssVariables } from "https://cdn.jsdelivr.net/npm/@jarida/ink@1.1.0/dist/index.js";
import { NavbarBehaviour, rove } from "https://cdn.jsdelivr.net/npm/@jarida/ink@1.1.0/dist/headless/index.js";

const root = document.documentElement;
const media = matchMedia("(prefers-color-scheme: dark)");
const KEY = "giap-theme";

function stored() {
  try { return localStorage.getItem(KEY); } catch { return null; }
}

function remember(value) {
  try { localStorage.setItem(KEY, value); } catch { /* private mode: the choice lasts this visit */ }
}

function scheme() {
  return root.dataset.theme || (media.matches ? "dark" : "light");
}

// Ink's tokens win over the stylesheet's fallbacks, except the ground, which stays Jarida Warm White.
function applyTheme() {
  const s = scheme();
  for (const [name, value] of Object.entries(cssVariables(createTheme({ scheme: s, accent: "Purple" })))) {
    if (name !== "--color-bg") root.style.setProperty(name, value);
  }
  root.dataset.scheme = s;
}

const saved = stored();
if (saved === "light" || saved === "dark") root.dataset.theme = saved;
applyTheme();
media.addEventListener("change", () => { if (!root.dataset.theme) applyTheme(); });

document.getElementById("theme-toggle")?.addEventListener("click", () => {
  root.dataset.theme = scheme() === "dark" ? "light" : "dark";
  remember(root.dataset.theme);
  applyTheme();
});

const bar = document.getElementById("bar");
if (bar) {
  const nav = new NavbarBehaviour();
  nav.subscribe((stage) => { bar.dataset.stage = stage; });
  const onScroll = () => nav.scrolled(window.scrollY);
  window.addEventListener("scroll", onScroll, { passive: true });
  onScroll();
  bar.dataset.stage = nav.current();
}

const tablist = document.getElementById("showcase-tabs");
if (tablist) {
  const tabs = [...tablist.querySelectorAll('[role="tab"]')];
  const select = (index, focus) => {
    tabs.forEach((tab, i) => {
      const on = i === index;
      tab.setAttribute("aria-selected", String(on));
      tab.tabIndex = on ? 0 : -1;
      document.getElementById(tab.getAttribute("aria-controls")).hidden = !on;
    });
    if (focus) tabs[index].focus();
  };
  tabs.forEach((tab, i) => tab.addEventListener("click", () => select(i, false)));
  tablist.addEventListener("keydown", (event) => {
    const current = tabs.findIndex((t) => t.getAttribute("aria-selected") === "true");
    const next = rove(event.key, current, tabs.length, { orientation: "horizontal", wrap: true });
    if (next === null) return;
    event.preventDefault();
    select(next, true);
  });
}
