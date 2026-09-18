# GIAP Desktop — Design System

> **Canonical approach**: HeroUI component library + shared primitives from `components/shared/` + token-based CSS classes in `styles/sections.css`. Design tokens live in `styles/design-tokens.css`.

---

## Rules

1. **No inline `style={{}}` for static values.** Move any style that does not change at runtime into a CSS class in `sections.css`.
2. **Dynamic-only exception.** Values driven by runtime state (audio levels, generated colors, computed positions) may remain inline.
3. **Token-first.** Always reference a design token (`var(--...)`) rather than a raw value.
4. **BEM-like naming.** CSS classes follow `.block__element--modifier` — one block per section file, elements and modifiers scoped beneath it.

---

## Design Tokens (`styles/design-tokens.css`)

All tokens are CSS custom properties on `:root`. HeroUI's stylesheet is imported first, then tokens override as needed.

### Typography

| Token | Value | Usage |
|---|---|---|
| `--font-display / --font-heading / --font-body` | Quicksand, Comfortaa | All UI text |
| `--font-mono` | JetBrains Mono | Code, timestamps |
| `--text-xs` | 10px | Group labels, badges |
| `--text-sm` | 12px | Captions, hints |
| `--text-base` | 13px | Body, nav items |
| `--text-md` | 15px | Toolbar titles |
| `--text-lg` | 18px | Card headings |
| `--text-xl` | 22px | Hero text |
| `--weight-regular/medium/semibold/bold` | 400/500/600/700 | |
| `--leading-tight/snug/base/loose` | 1.15/1.25/1.5/1.7 | |

### Brand Colors

| Token | Value | Usage |
|---|---|---|
| `--color-accent` | `#8C4BFF` | Jarida Purple — primary CTA, active states |
| `--color-accent-hover` | `#7636E6` | Hover on accent elements |
| `--color-accent-soft` | `rgba(140,75,255,0.11)` | Selected backgrounds |
| `--color-text` | `#171616` | Jarida Charcoal — all body text |
| `--color-text-secondary` | `rgba(23,22,22,0.55)` | Hints, labels |
| `--color-text-tertiary` | `rgba(23,22,22,0.38)` | Placeholders, disabled |
| `--color-bg` | `#FFF9F9` | Main window background |
| `--color-surface` | `#ffffff` | Cards, inputs, dropdowns |

### Semantic Colors

| Token | Color | Soft bg |
|---|---|---|
| `--color-success` | `#34C759` | `--color-success-soft` |
| `--color-warning` | `#FF9500` | `--color-warning-soft` |
| `--color-destructive` | `#FF3B30` | `--color-destructive-soft` |

### Grey Scale

`--grey-50` through `--grey-900` (zinc-based). Use for borders (`--grey-200`), muted text (`--grey-500 / --grey-600`), and surface fills (`--grey-100`).

### Spacing (8pt grid)

`--space-1` (4px) → `--space-16` (64px). Use these for all padding, margin, and gap values.

### Border Radius

| Token | Value | Usage |
|---|---|---|
| `--radius-sm` | 6px | Small chips |
| `--radius-md` | 10px | Inputs, buttons |
| `--radius-lg` | 14px | Cards |
| `--radius-pill` | 999px | Tags, status dots |
| `--radius-card` | 14px | Alias for card corners |

### Layout

| Token | Value |
|---|---|
| `--toolbar-height` | 48px |
| drawer width | 344px (a literal, not a token — one component owns it) |
| `--row-height-sm/md/lg` | 36px / 44px / 52px |

### Shadows

`--shadow-xs/sm/md/lg` — warm-toned, restrained. Use `--shadow-canvas` for floating panels and `--shadow-brand` for accent glow effects.

---

## Shared Primitives (`components/shared/`)

Import from `"../components/shared"`.

### `<PageHeader title action? />`

Top-of-screen heading row with optional right-side action slot.

```tsx
import { PageHeader } from "../components/shared";

<PageHeader
  title="Memory"
  action={<Button variant="primary">Add</Button>}
/>
```

### `<Section title>`

Card wrapper for a labelled settings block. Contains `<Row>` children.

```tsx
<Section title="Provider">
  <Row label="Model" hint="Used for all chat sessions">
    <Select ... />
  </Row>
</Section>
```

### `<Row label hint? />`

Two-column layout row: label + optional hint on the left, control on the right.

```tsx
<Row label="Temperature" hint="Higher = more creative">
  <input type="range" ... />
</Row>
```

### `<EmptyState icon inline? />`

Centred empty-state with icon + text. Pass `inline` for compact inline variant.

```tsx
<EmptyState icon={<Brain size={20} />}>No memories yet.</EmptyState>
```

### `<Metric label value trend? />`

KPI tile. `trend` accepts `"up"` | `"down"` for coloured modifier.

```tsx
<Metric label="Sessions today" value="12" trend="up" />
```

### `<QuickAction icon label onPress />`

Arrow-linked action row used on Dashboard.

```tsx
<QuickAction icon={<Plus size={16} />} label="New chat" onPress={...} />
```

### `<RoleChip role model color icon />`

Coloured chip displaying an LLM role assignment. `color` maps to `role-chip--{color}` modifier.

```tsx
<RoleChip role="Chat" model="llama3.2" color="chat" icon={<MessageCircle size={14} />} />
```

### `useConfirm()`

Promise-based confirm dialog. Wrap the app in `<ConfirmProvider>` (already done in `App.tsx`).

```tsx
const confirm = useConfirm();

async function handleDelete() {
  if (!await confirm("Delete this item?", { title: "Delete", destructive: true })) return;
  // proceed
}
```

---

## HeroUI Components

Use HeroUI (`@heroui/react`) for interactive widgets: `Button`, `Input`, `Switch`, `Tabs`, `Card/CardContent`, `Chip`, `Modal`.

### Button Variants

All variant overrides are defined in `sections.css` (unlayered, loads last) to reliably beat HeroUI's `@layer theme` defaults.

| `variant` prop | Appearance | Use for |
|---|---|---|
| `"primary"` | Jarida Purple fill | Primary CTA (Add, Save, Confirm) |
| `"secondary"` | Grey fill, grey border | Secondary actions (Refresh, Cancel) |
| `"danger-soft"` | Red soft fill, red border | Destructive (Delete) |
| `"outline"` | Transparent, grey border | Tertiary / toolbar buttons |
| `"ghost"` | Fully transparent | Icon-only utility buttons |
| `"light"` | HeroUI default light | Low-emphasis actions |

```tsx
<Button variant="primary" size="sm">Save</Button>
<Button variant="secondary" size="sm">Cancel</Button>
<Button variant="danger-soft" size="sm">Delete all</Button>
<Button isIconOnly variant="ghost" size="sm"><Trash2 size={15} /></Button>
```

**Sizing note:** `sections.css` sets a `min-width` floor on non-icon buttons (`sm` → 80px, `md` → 96px, `lg` → 112px) for uniform button widths. Use `isIconOnly` for icon-only buttons to opt out of this floor.

### Card

Always use the `.card` className alongside HeroUI `<Card>` so the shared border/radius/shadow override applies.

```tsx
<Card className="card">
  <CardContent>...</CardContent>
</Card>
```

For list-style card bodies use `.card-body--list`; for flush (no-padding) use `.card-body--flush`.

---

## CSS Class Conventions

### Block naming

Each section gets its own block prefix matching the section name:

| Section | Prefix |
|---|---|
| Memory | `.mem-` |
| Chat | `.bubble-`, `.chat-` |
| Dashboard | `.dash-`, `.voice-card-`, `.server-row-` |
| Schedules | `.sched-` |
| Canvas | `.canvas-` |
| Agent | `.agent-` |
| Logs | `.logs-` |
| Skills | `.skill-row__`, `.skills-form__` |

### Modifiers

State modifiers use `is-*` on the element itself:

```css
.chat-dock.is-collapsed { ... }
.wave.is-playing { ... }
```

Structural modifiers use `--modifier` suffix:

```css
.empty-state--inline { ... }
.card-body--flush { ... }
.text-error--sm { ... }
```

### Utility classes

A small set of utilities is defined in `base.css` and re-used across sections:

| Class | Effect |
|---|---|
| `.muted-12` | `font-size: 12px; color: var(--grey-500)` |
| `.text-error` | `color: var(--color-destructive)` |
| `.text-error--sm` | error text at 12px |
| `.truncate` | ellipsis overflow |
| `.select-text` | enables text selection |
| `.animate-fade-in` | fade + slide up entrance |
| `.animate-spin` | continuous rotation |
