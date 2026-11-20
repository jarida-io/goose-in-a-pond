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

const tablist = document.getElementById("look-tabs");
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

for (const list of document.querySelectorAll(".look-list")) {
  const items = [...list.querySelectorAll(".look-item")];
  const shots = [...list.closest(".look-panel").querySelectorAll(".dev-glass img[data-shot]")];
  list.addEventListener("click", (event) => {
    const pill = event.target.closest(".look-pill");
    if (!pill) return;
    for (const item of items) {
      const on = item.contains(pill);
      item.dataset.open = String(on);
      item.querySelector(".look-pill").setAttribute("aria-pressed", String(on));
    }
    for (const img of shots) img.dataset.on = String(img.dataset.shot === pill.dataset.shot);
  });
}

const highlights = document.getElementById("hl");
if (highlights) carousel(highlights);

function carousel(root) {
  const track = root.querySelector(".hl-track");
  const cards = [...track.querySelectorAll(".hl-card")];
  const controls = root.querySelector(".hl-controls");
  const dots = root.querySelector(".hl-dots");
  const play = root.querySelector(".hl-play");
  const still = matchMedia("(prefers-reduced-motion: reduce)");
  const pad = () => parseFloat(getComputedStyle(track).paddingLeft);
  let index = -1;
  // While a dot or the timer is steering, the cards the scroll passes over must not take the mark.
  let steerUntil = 0;
  let queued = false;

  const buttons = cards.map((card, i) => {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "hl-dot";
    button.setAttribute("aria-label", `${i + 1} of ${cards.length}: ${card.querySelector(".hl-cap").innerText.replace(/\s+/g, " ")}`);
    button.innerHTML = '<span class="hl-pip"><span class="hl-fill"></span></span>';
    button.addEventListener("click", () => { setPlaying(false); go(i); });
    dots.append(button);
    return button;
  });

  function mark(i) {
    if (i === index) return;
    index = i;
    buttons.forEach((button, j) => button.setAttribute("aria-current", String(j === i)));
  }

  function go(i) {
    mark(i);
    steerUntil = performance.now() + 900;
    track.scrollTo({ left: cards[i].offsetLeft - pad(), behavior: still.matches ? "auto" : "smooth" });
  }

  function sync() {
    queued = false;
    if (performance.now() < steerUntil) return;
    const x = track.scrollLeft + pad();
    let best = 0;
    cards.forEach((card, i) => {
      if (Math.abs(card.offsetLeft - x) < Math.abs(cards[best].offsetLeft - x)) best = i;
    });
    mark(best);
  }

  function setPlaying(on) {
    root.dataset.playing = String(on);
    play.setAttribute("aria-label", on ? "Pause the highlights" : "Play the highlights");
  }

  track.addEventListener("scroll", () => {
    if (!queued) { queued = true; requestAnimationFrame(sync); }
  }, { passive: true });
  track.addEventListener("scrollend", () => { steerUntil = 0; sync(); });

  track.addEventListener("pointerdown", () => setPlaying(false));
  track.addEventListener("keydown", (event) => {
    if (["ArrowLeft", "ArrowRight", "Home", "End", "PageUp", "PageDown"].includes(event.key)) setPlaying(false);
  });
  track.addEventListener("wheel", (event) => {
    if (Math.abs(event.deltaX) > Math.abs(event.deltaY)) setPlaying(false);
  }, { passive: true });

  dots.addEventListener("animationend", (event) => {
    if (event.animationName !== "hl-fill" || root.dataset.playing !== "true") return;
    go((index + 1) % cards.length);
  });
  play.addEventListener("click", () => setPlaying(root.dataset.playing !== "true"));

  // The timer only runs while the carousel is on screen, so nobody returns to slide five of seven.
  root.dataset.visible = "false";
  new IntersectionObserver(([entry]) => {
    root.dataset.visible = String(entry.isIntersecting && entry.intersectionRatio >= 0.4);
  }, { threshold: 0.4 }).observe(track);

  setPlaying(!still.matches);
  mark(0);
  controls.hidden = false;
}
