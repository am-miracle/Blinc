# cn components — visual fidelity audit & plan

## TL;DR

`CN_STYLES` hardcodes ~50 pixel values for radii, padding, font-sizes
and transition durations. The Universal HID themes carry per-variant
`RadiusTokens` / `SpacingTokens` / `TypographyTokens` /
`AnimationTokens` / `ShapeTokens` ladders, but **none of those four
token families are exposed as CSS variables today** — only colour
tokens reach the cascade. The CSS rules therefore can't pick up the
active theme's geometry / motion ladder, so Restrained / Hybrid /
Expressive look identical for radii, paddings and durations.

The fix is two-step:

1. **Plumbing.** Extend `ThemeState::to_css_variable_map` to emit
   the missing token families (`--radius-*`, `--space-*`, `--text-*`,
   `--duration-*`, `--ease-*`). Then rewrite the hardcoded CN_STYLES
   values as `var(--*)` references.

2. **Per-widget polish.** Once tokens flow, walk each cn widget and
   confirm the variant / token mapping is correct (e.g. `.cn-card`
   uses `radius_xl` not `radius_default`; `.cn-tabs-trigger--md`
   reads `text_sm`, not 14px; etc.).

The audit below catalogues every CN_STYLES override that currently
ignores a theme token, with the variant-specific expected value for
Hybrid / Restrained / Expressive.

---

## Background — what CSS variables exist today

`ThemeState::to_css_variable_map()` emits **only colour tokens** —
`--primary`, `--surface`, `--text-primary`, etc. (44 colours total).
Radii, spacing, typography sizes, durations, easings, shape — none
of them are exposed. That's why `.cn-card { border-radius: 12px; }`
is a literal `12px` and not `var(--radius-xl)`.

The Universal HID variants ship the following ladders (all in
`themes/universal/*.rs`):

| Token | Restrained | Hybrid (default) | Expressive |
| --- | --- | --- | --- |
| `radius_sm` | 3 | 4 | 4 |
| `radius_default` | 6 | 8 | 10 |
| `radius_md` | 8 | 10 | 12 |
| `radius_lg` | 10 | 14 | 16 |
| `radius_xl` | 14 | 18 | 24 |
| `radius_2xl` | 18 | 24 | 28 |
| `radius_3xl` | 24 | 32 | 36 |
| `duration_fastest` | 75 ms | 80 ms | 100 ms |
| `duration_faster` | 100 ms | 120 ms | 150 ms |
| `duration_fast` | 150 ms | 180 ms | 200 ms |
| `duration_normal` | 200 ms | 240 ms | 280 ms |
| `duration_slow` | 280 ms | 320 ms | 400 ms |
| `text_base` | 15 | 15 | 15 |
| Spacing | 4-px scale (shared) | same | same |
| Shape | smoothing 0.65 / exp 4.4 | 0.40 / 3.3 | 0.20 / 2.6 |

Restrained's `radius_default` is **6** and Expressive's is **10**.
CN_STYLES hardcodes **6** for almost every widget radius — so
Restrained matches by accident, Hybrid is off by 2, Expressive by 4.

---

## Per-widget audit

The table below lists every CN_STYLES geometry / motion value that
ignores a theme token. "Expected" columns show what each Universal
HID variant prescribes through its `RadiusTokens` /
`AnimationTokens` / `SpacingTokens` / `TypographyTokens`. ✖ = the
hardcoded value diverges from the theme's expectation for at least
one variant. ✓ = the hardcoded value happens to match all three
variants.

### Button

| Property | CN_STYLES | Restrained | Hybrid | Expressive | Status |
| --- | --- | --- | --- | --- | --- |
| `.cn-button` base radius | `6px` | `radius_default` = 6 | 8 | 10 | ✖ Hybrid + Expressive |
| `.cn-button--sm` radius | `4px` | `radius_sm` = 3 | 4 | 4 | ✖ Restrained |
| `.cn-button--md` radius | `6px` | `radius_default` = 6 | 8 | 10 | ✖ Hybrid + Expressive |
| `.cn-button--lg` radius | `8px` | `radius_lg` = 10 | 14 | 16 | ✖ all three |
| `.cn-button--icon` radius | `6px` | `radius_default` = 6 | 8 | 10 | ✖ Hybrid + Expressive |

Note: `button.rs` already reads `theme.radii().get(radius_token())`
in Rust (committed in `75a2f154`), so the **Rust side is correct
per-variant** — the CSS rule above is the cascade override that
beats the Rust default. Need to either drop the hardcoded values or
make them `var(--radius-default)` etc.

### Card

| Property | CN_STYLES | Restrained | Hybrid | Expressive | Status |
| --- | --- | --- | --- | --- | --- |
| `.cn-card` radius | `12px` | `radius_xl` = 14 | 18 | 24 | ✖ all three |
| `.cn-card` padding | `24px` | `space_6` = 24 | 24 | 24 | ✓ |
| `.cn-card` gap | `16px` | `space_4` = 16 | 16 | 16 | ✓ |
| `.cn-card-header` gap | `6px` | `space_1_5` = 6 | 6 | 6 | ✓ |
| `.cn-card-footer` gap | `8px` | `space_2` = 8 | 8 | 8 | ✓ |

Cards land closest to **Hybrid**'s `radius_md` (10) — not the
intended `radius_xl` for a primary surface. Expressive cards
visibly under-rounded vs the variant's intent (24).

### Badge

| Property | CN_STYLES | Status |
| --- | --- | --- |
| `.cn-badge` radius | `9999px` | ✓ (pill — radius_full intent) |
| `.cn-badge` font-size | `12px` | `text_xs` = 12 ✓ |
| `.cn-badge` padding | `2px 10px` | hardcoded — no token equivalent for `2/10` (space_0_5 = 2, ~space_2_5 = 10) ✓ |

Badge geometry is fine. Background-fill bug seen in screenshots
was the demo running without CN_STYLES; with CN_STYLES present,
badge variants pick up `var(--primary)` / `var(--success)` etc.
correctly.

### Alert

| Property | CN_STYLES | Restrained | Hybrid | Expressive | Status |
| --- | --- | --- | --- | --- | --- |
| `.cn-alert` radius | `6px` | `radius_default` = 6 | 8 | 10 | ✖ Hybrid + Expressive |
| `.cn-alert` padding | `16px` | `space_4` = 16 | 16 | 16 | ✓ |
| `.cn-alert` font-size | `14px` | `text_sm` = 13 (themes set 13!) | 13 | 13 | ✖ all three (hardcoded 14 vs theme 13) |
| `.cn-alert-box` gap | `12px` | `space_3` = 12 | 12 | 12 | ✓ |

Note: the Universal themes deliberately tune `text_sm` to **13**
for HID density. CN_STYLES's `14px` overrides that for every
secondary-text widget.

### Input / Textarea

| Property | CN_STYLES | Restrained | Hybrid | Expressive | Status |
| --- | --- | --- | --- | --- | --- |
| `.cn-input` radius | `6px` | `radius_default` = 6 | 8 | 10 | ✖ Hybrid + Expressive |
| `.cn-textarea` radius | `6px` | same | 8 | 10 | ✖ Hybrid + Expressive |
| `.cn-input--sm` font-size | `12px` | `text_xs` = 12 ✓ |
| `.cn-input--md` font-size | `14px` | `text_sm` = 13 | 13 | 13 | ✖ all three |
| `.cn-input--lg` font-size | `16px` | `text_lg` = 17 (themes set 17) | 17 | 17 | ✖ all three |

### Tabs

| Property | CN_STYLES | Restrained | Hybrid | Expressive | Status |
| --- | --- | --- | --- | --- | --- |
| `.cn-tabs-list` radius | `8px` | `radius_default` = 6 | 8 | 10 | ✖ Restrained + Expressive |
| `.cn-tabs-list` padding | `6px` | `space_1_5` = 6 | 6 | 6 | ✓ |
| `.cn-tabs-list` gap | `4px` | `space_1` = 4 | 4 | 4 | ✓ |
| `.cn-tabs-trigger` radius | `6px` | `radius_default` = 6 | 8 | 10 | ✖ Hybrid + Expressive |
| `.cn-tabs-trigger:hover` transition | `150ms` | `duration_fast` = 150 | 180 | 200 | ✖ Hybrid + Expressive |
| `.cn-tabs-trigger--sm` height | `32px` | hardcoded (no height token) | | | ⚠ no token |
| `.cn-tabs-trigger--md` font-size | `14px` | `text_sm` = 13 | 13 | 13 | ✖ |

### Select / Combobox

| Property | CN_STYLES | Restrained | Hybrid | Expressive | Status |
| --- | --- | --- | --- | --- | --- |
| `.cn-select-trigger` radius | `6px` | 6 | 8 | 10 | ✖ Hybrid + Expressive |
| `.cn-select-content` radius | `8px` | `radius_md` = 8 | 10 | 12 | ✖ Hybrid + Expressive |
| `.cn-select-item` radius | `4px` | `radius_sm` = 3 | 4 | 4 | ✖ Restrained |
| `.cn-select-item` padding | `8px 12px` | `space_2` / `space_3` | same | same | ✓ |
| `.cn-select-item` transition | `100ms` | hardcoded (`duration_fastest` = 75 / 80 / 100) | | | ✖ all three |

Hover transition speed differs between Restrained (75ms) and
Expressive (100ms) — but `.cn-select-item:hover` is locked to
100ms regardless.

### Tooltip

| Property | CN_STYLES | Theme expectation | Status |
| --- | --- | --- | --- |
| `.cn-tooltip` radius | `4px` | `radius_sm` Restrained=3 / Hybrid=4 / Expressive=4 | ✖ Restrained |
| `.cn-tooltip` font-size | `12px` | `text_xs` = 12 | ✓ |
| `.cn-tooltip` padding | `6px 12px` | `space_1_5` / `space_3` | ✓ |

### Dialog / Drawer / Sheet / Toast

| Property | CN_STYLES | Restrained | Hybrid | Expressive | Status |
| --- | --- | --- | --- | --- | --- |
| `.cn-dialog` radius | `12px` | `radius_xl` = 14 | 18 | 24 | ✖ all three |
| `.cn-dialog` padding | `24px` | `space_6` = 24 | 24 | 24 | ✓ |
| `.cn-dialog` gap | `16px` | `space_4` = 16 | 16 | 16 | ✓ |
| `.cn-drawer-header` padding | `16px` | `space_4` = 16 | 16 | 16 | ✓ |
| `.cn-toast` radius | `12px` | `radius_xl` = 14 | 18 | 24 | ✖ all three |
| `.cn-toast--*` accent border | `4px` | hardcoded (no token) | | | ⚠ no token |

Dialogs / toasts are "hero" surfaces — they should follow the bold
end of each variant's radius ladder (`radius_xl` or higher).
Currently locked to `12px` regardless of theme.

### Accordion

| Property | CN_STYLES | Restrained | Hybrid | Expressive | Status |
| --- | --- | --- | --- | --- | --- |
| `.cn-accordion` radius | `12px` | `radius_xl` = 14 | 18 | 24 | ✖ all three |
| `.cn-accordion-trigger` padding | `16px 12px` | `space_4` / `space_3` | same | same | ✓ |
| `.cn-accordion-trigger` font-size | `14px` | `text_sm` = 13 | 13 | 13 | ✖ all three |

### Dropdown / Context menu / Menubar / Popover / Hover card

These all use the same overlay-panel chrome — and they all hardcode
the same values.

| Property | CN_STYLES | Restrained | Hybrid | Expressive | Status |
| --- | --- | --- | --- | --- | --- |
| Panel radius | `8px` | `radius_md` = 8 | 10 | 12 | ✖ Hybrid + Expressive |
| Panel padding | `4px` (menu) / `16px` (popover) | `space_1` / `space_4` | same | same | ✓ |
| Item radius | `4px` | `radius_sm` = 3 | 4 | 4 | ✖ Restrained |
| Item padding | `8px 12px` | `space_2` / `space_3` | same | same | ✓ |
| Item transition | `100ms` | `duration_fastest` 75 / 80 / 100 | | | ✖ Restrained + Hybrid |
| Item font-size | `14px` | `text_sm` = 13 | 13 | 13 | ✖ all three |

### Skeleton

| Property | CN_STYLES | Status |
| --- | --- | --- |
| `.cn-skeleton` radius | `4px` | `radius_sm` Restrained=3 / Hybrid=4 / Expressive=4 → ✖ Restrained |

### Avatar

| Property | CN_STYLES | Status |
| --- | --- | --- |
| `.cn-avatar` radius | `9999px` (pill) | ✓ |
| `.cn-avatar--square` radius | `6px` | `radius_default` Restrained=6, Hybrid=8, Expressive=10 → ✖ Hybrid + Expressive |

### Checkbox / Switch / Radio / Slider / Progress

These mostly use `radius: 9999px` (pill / true round) which is the
correct intent for switches, radios, sliders. Fine.

| Property | CN_STYLES | Status |
| --- | --- | --- |
| `.cn-checkbox` radius | `4px` | ✖ Restrained (expects 3) |
| `.cn-checkbox` transition | `150ms / 100ms` | ✖ varies |

### Pagination

| Property | CN_STYLES | Status |
| --- | --- | --- |
| `.cn-pagination` gap | `4px` | ✓ |
| `.cn-pagination-btn` radius | `6px` | ✖ Hybrid + Expressive |

---

## Surface / background colour semantics

Colour tokens **are** already exposed as CSS variables, so CN_STYLES
uses `var(--surface)` etc. correctly *syntactically*. The issue is
semantic — many widgets pick the **wrong tier** in the four-level
surface ladder:

| Token | Hybrid Light | Hybrid Dark | Intent |
| --- | --- | --- | --- |
| `--background` | `#F6F8FC` | `#0F1320` | Page canvas — the lowest tier |
| `--surface` | `#FFFFFF` | `#1A1F2E` | Elevation 1 — primary container (card body) |
| `--surface-elevated` | `#FBFCFE` | `#232940` | Elevation 2 — modals, dropdowns, active selections |
| `--surface-overlay` | `#E7ECF6` | `#0A0D17` | Tonal / ephemeral — accent-tinted backgrounds (tab strip, pill containers) |

Looking at CN_STYLES against that ladder:

| Widget | CN_STYLES today | Should be | Why |
| --- | --- | --- | --- |
| `.cn-card` | `--surface` | `--surface` | ✓ Correct (elevation 1) |
| `.cn-dialog` | `--surface` | `--surface-elevated` | ✖ Modals sit above the surface plane → elevation 2 |
| `.cn-drawer` | `--surface` | `--surface-elevated` | ✖ Same — modal overlay |
| `.cn-sheet` | `--surface` | `--surface-elevated` | ✖ Same |
| `.cn-toast` | `--surface` | `--surface-elevated` | ✖ Floating notification = elevation 2 |
| `.cn-popover-content` | `--surface` | `--surface-elevated` | ✖ Popover floats above the page |
| `.cn-hover-card-content` | `--surface` | `--surface-elevated` | ✖ Same |
| `.cn-dropdown-menu` | `--surface` | `--surface-elevated` | ✖ Dropdown panel floats |
| `.cn-context-menu` | `--surface` | `--surface-elevated` | ✖ Same |
| `.cn-menubar` | `--surface` | `--surface-elevated` | ✖ The menubar BAR itself is an elevated container |
| `.cn-select-content` | `--surface` | `--surface-elevated` | ✖ Dropdown panel |
| `.cn-combobox-content` | `--surface` | `--surface-elevated` | ✖ Same |
| `.cn-sidebar` | `--surface` | `--surface` | ✓ Sidebar is a container, not a float |
| `.cn-tabs-list` | `--surface-elevated` | `--surface-overlay` | ✖ Tab strip is a tonal tray; active tab pops out via `--surface` |
| `.cn-tabs-trigger--active` | `--background` | `--surface` | ✖ "background" is the canvas — using it for the active-tab fill makes the tab visually MERGE with the canvas instead of standing out from the elevated strip |
| `.cn-accordion` | `--surface-elevated` | `--surface` | ✖ Accordion outer is a card-like container, not a modal — should be elevation 1 |
| `.cn-accordion-content` | `--surface` | `--background` | ✖ Inside an elevated container, content should recede (canvas tone) |
| `.cn-accordion-trigger:hover` | `--surface-overlay` | `--surface-overlay` | ✓ Tonal hover read |
| `.cn-skeleton` | `--surface-elevated` | `--surface-elevated` | ✓ |
| `.cn-slider-track` | `--surface-elevated` | `--surface-overlay` | △ Either works; overlay would tint with accent on Expressive |
| `.cn-tabs-trigger:hover` | `--surface-overlay` | `--surface-overlay` | ✓ |
| `.cn-select-item:hover` | `--surface-elevated` | `--surface-elevated` | △ Items inside an elevated panel hovering to a STILL-elevated colour means low contrast |

**The pattern that breaks:** in HIG / Material 3 design language,
floating elements should be ONE STEP UP from their parent. The cn
library currently uses `--surface` for both the card body AND its
floating dialog, which means dialogs in dark Hybrid sit at the
exact same `#1A1F2E` as the page's content cards — no visual lift.

**Tabs are the most visibly wrong** — the active tab uses
`--background` against an `--surface-elevated` list. On Hybrid
Dark, that's `#0F1320` (very dark) on `#232940` (lighter) — the
active tab appears DARKER than the list, the opposite of the
intended "raised selection" read.

### Recommended surface-token reshuffle

Four targeted CN_STYLES edits (Phase 3 work):

```css
.cn-dialog, .cn-drawer, .cn-sheet, .cn-toast,
.cn-popover-content, .cn-hover-card-content,
.cn-dropdown-menu, .cn-context-menu, .cn-menubar,
.cn-select-content, .cn-combobox-content {
    background: var(--surface-elevated);   /* was --surface */
}

.cn-tabs-list { background: var(--surface-overlay); }       /* was --surface-elevated */
.cn-tabs-trigger--active { background: var(--surface); }     /* was --background */
.cn-accordion { background: var(--surface); }                /* was --surface-elevated */
.cn-accordion-content { background: var(--background); }     /* was --surface */
```

That single reshuffle gives every Universal HID variant the
correct visual hierarchy on the four-tier surface ladder.

---

## Typography — full audit

Font-size was covered above. The other typography axes are
**unused entirely** in CN_STYLES today:

| Axis | CN_STYLES | Theme tokens available | Status |
| --- | --- | --- | --- |
| `font-family` | never set | `font_sans` (`"Noto Sans" + system stack`), `font_mono`, `font_serif` | ✖ Universal HID's deliberate font choice never reaches cn widgets |
| `font-weight` | never set | `font_thin` (100), `font_light` (300), `font_normal` (400), `font_medium` (500), `font_semibold` (600), `font_bold` (700), `font_black` (900) | ✖ Rust side calls `.medium()` / `.semibold()` directly with literal weight enums; no theme override path |
| `line-height` | never set | `leading_none` (1.0), `leading_tight` (1.2), `leading_snug` (1.35), `leading_normal` (1.5), `leading_relaxed` (1.625), `leading_loose` (2.0) | ✖ No widget reads these |
| `letter-spacing` | never set | `tracking_tighter` (-0.04), `tracking_tight` (-0.02), `tracking_normal` (0), `tracking_wide` (0.025), `tracking_wider` (0.05) em | ✖ No widget reads these |

### Per-axis impact

**font-family** is the highest-impact gap. The Universal HID design
deliberately promotes **Noto Sans** to the canonical sans across
all three variants — "it's already the framework's universal
fallback on platforms where the system font isn't available, and
using it consistently means the universal theme renders the same
regardless of platform font resolution" (from the design doc).
Today every cn widget falls through to the renderer's default
font stack (whatever the platform picks), not Noto Sans.

**font-weight** — cn widgets call `.medium()` / `.semibold()` /
`.bold()` directly via the layout text builder, hardcoding 500 /
600 / 700. Universal themes ship the same numeric weights but
there's no path for an app to remap them per-theme (e.g. a custom
theme that wants `font_medium = 450` for a lighter-feeling UI
couldn't get cn widgets to follow).

**line-height** — the layout text widget uses an internal default
(~`1.4`). Universal HID's `leading_normal = 1.5` and `leading_snug
= 1.35` aren't picked up. Most visible on accordion content +
alert description (multi-line text).

**letter-spacing** — Hybrid's `tracking_tight = -0.02 em` is a
deliberate dense-HID feel that no cn widget applies. Display-size
headings (card title, dialog title) would benefit from
`tracking_tighter` for the classic "tight at large sizes" rule.

### Rust-side typography binding

Beyond CSS, cn widgets construct text in Rust without consulting
the theme. Example from `card.rs`:

```rust
text(title).size(16.0).semibold()        // hardcoded
```

Universal HID's `text_base = 15`. The card title should be
`text_lg` (17) or `text_xl` (20), not a literal 16. Same gap for
every `.size(N.0)` call in cn widget files.

### Recommended typography work (Phase 1 extension + Phase 3 widget audit)

Add to `to_css_variable_map`:

```
--font-sans, --font-mono, --font-serif       (font-family stacks)
--font-thin, --font-light, --font-normal,
--font-medium, --font-semibold, --font-bold, --font-black  (weights, numeric)
--leading-none, --leading-tight, --leading-snug,
--leading-normal, --leading-relaxed, --leading-loose       (line-heights, unitless)
--tracking-tighter, --tracking-tight, --tracking-normal,
--tracking-wide, --tracking-wider                          (letter-spacing, em)
```

Then in CN_STYLES, add a base rule that applies the font-family
once for every cn widget (avoids per-rule repetition):

```css
.cn-button, .cn-card, .cn-input, .cn-textarea, .cn-tabs-trigger,
.cn-alert, .cn-badge, .cn-menubar, .cn-dropdown-menu, .cn-context-menu,
.cn-popover-content, .cn-tooltip, .cn-dialog, .cn-drawer, .cn-sheet,
.cn-toast, .cn-accordion, .cn-breadcrumb, .cn-pagination,
.cn-nav-link, .cn-sidebar-item, .cn-tree-node, .cn-select-trigger,
.cn-combobox-trigger {
    font-family: var(--font-sans);
}

.cn-kbd {
    font-family: var(--font-mono);
}
```

For body / paragraph-style widgets, add `line-height` so multi-line
content reads correctly:

```css
.cn-alert, .cn-alert-box, .cn-card-content, .cn-accordion-content,
.cn-tooltip {
    line-height: var(--leading-normal);
}
```

For the dense-HID feel of Universal Hybrid / Restrained:

```css
.cn-button, .cn-tabs-trigger, .cn-input, .cn-select-trigger {
    letter-spacing: var(--tracking-tight);
}
```

### Rust-side typography (Phase 3)

Walk every cn widget that calls `text(...).size(N)` / `.weight(W)`
directly and replace with `theme.typography().get(TypographyToken::*)`
or the corresponding field access. Approximate count:

- `card.rs` — 4 `.size()` calls
- `dialog.rs` — 3
- `alert.rs` — 3
- `button.rs` — 2 (label + icon)
- `badge.rs` — 1
- `tooltip.rs` — 1
- `input.rs` / `textarea.rs` — placeholder + value text
- `tabs.rs` — 1 per trigger size
- `menubar.rs` / `dropdown_menu.rs` / `context_menu.rs` — item label
- `accordion.rs` — trigger + content
- `breadcrumb.rs` / `pagination.rs` — link / button text
- `sidebar.rs` / `nav.rs` — item label
- ~40 call sites total

Each call site replaces the literal with `theme.typography().text_sm`
etc., picking the semantic role appropriate for the widget.

---

## Summary of inconsistency categories

1. **Hero-surface radii are pinned to `12px`** (card / dialog /
   toast / accordion). The Universal variants want `radius_xl`
   = 14 / 18 / 24 — Expressive in particular is dramatically
   under-rounded.

2. **Body-text font-size is pinned to `14px`** across alert /
   menubar item / accordion trigger / dropdown item / input--md
   / tabs--md. The Universal variants set `text_sm = 13` for HID
   density. Currently always one px larger than designed.

3. **Large-text font-size is pinned to `16px`** (`.cn-input--lg`)
   vs the theme's `text_lg = 17`.

4. **Default radii pinned to `6px`** (button / select trigger /
   input / textarea / tooltip / popover trigger / etc.). Matches
   Restrained but Hybrid wants 8 and Expressive wants 10.

5. **Small radii pinned to `4px`** (item radius for select /
   dropdown / context / menubar / combobox / pagination /
   skeleton). Hybrid + Expressive's `radius_sm = 4` matches by
   accident; Restrained wants 3.

6. **Transitions pinned to `100ms` / `150ms`** ignoring
   `duration_fastest` (75 / 80 / 100) and `duration_fast` (150 /
   180 / 200) variant differences. Restrained should feel snappier
   than Expressive; currently they feel identical.

7. **Shape (squircle)** — none of the CN_STYLES emit
   `corner-shape:` declarations, so squircle is applied only via
   the Rust paint-walker substitution (the work in commits
   `e90791a8` + `e0981712`). This actually works as designed —
   the squircle is theme-driven and not overridable by CN_STYLES.
   ✓

---

## Proposed plan — three phases, each its own commit

### Phase 1 — Expose theme tokens as CSS variables

Extend [`ThemeState::to_css_variable_map`](crates/blinc_theme/src/state.rs#L412)
to emit every non-colour token family. ~60 new variables.

**Radii** (px):
```
--radius-none, --radius-sm, --radius-default, --radius-md, --radius-lg,
--radius-xl, --radius-2xl, --radius-3xl, --radius-full
```

**Spacing** (px):
```
--space-0, --space-0-5, --space-1, --space-1-5, --space-2, --space-2-5,
--space-3, --space-3-5, --space-4, --space-5, --space-6, --space-7,
--space-8, --space-9, --space-10, --space-11, --space-12, --space-14,
--space-16, --space-20, --space-24, --space-28, --space-32
```

**Typography — font families** (CSS font-family stacks):
```
--font-sans, --font-mono, --font-serif
```

**Typography — sizes** (px):
```
--text-xs, --text-sm, --text-base, --text-lg, --text-xl,
--text-2xl, --text-3xl, --text-4xl, --text-5xl
```

**Typography — weights** (numeric, unitless):
```
--font-thin, --font-light, --font-normal, --font-medium,
--font-semibold, --font-bold, --font-black
```

**Typography — line-heights** (unitless multiplier):
```
--leading-none, --leading-tight, --leading-snug,
--leading-normal, --leading-relaxed, --leading-loose
```

**Typography — letter-spacing** (em):
```
--tracking-tighter, --tracking-tight, --tracking-normal,
--tracking-wide, --tracking-wider
```

**Motion — durations** (ms):
```
--duration-fastest, --duration-faster, --duration-fast,
--duration-normal, --duration-slow, --duration-slower, --duration-slowest
```

**Motion — easings** (CSS `cubic-bezier(...)` or keywords):
```
--ease-default, --ease-in, --ease-out, --ease-in-out
```

(Skip `--shape-*` — the squircle is paint-walker-driven, not CSS.)

Files touched:
- `crates/blinc_theme/src/state.rs` — extend the hashmap builder
  (~60 new `vars.insert(...)` lines plus serialisers for
  `FontFamily` and `Easing`)
- `crates/blinc_theme/src/state.rs` — tests verifying the keys
  exist after init

No behaviour change for existing apps — they don't reference
these variables yet. Pure additive.

### Phase 2 — Rewrite CN_STYLES to reference theme variables

Replace every hardcoded geometry / motion value in
[`cn_styles.rs`](crates/blinc_cn/src/cn_styles.rs) with
`var(--*)` references. Concrete substitutions, in order of
visual impact:

| Find | Replace with |
| --- | --- |
| `border-radius: 12px;` (card/dialog/toast/accordion) | `var(--radius-xl)` |
| `border-radius: 8px;` (overlay panels / tabs-list) | `var(--radius-md)` |
| `border-radius: 6px;` (default-sized widgets) | `var(--radius-default)` |
| `border-radius: 4px;` (small items) | `var(--radius-sm)` |
| `font-size: 14px;` | `var(--text-sm)` |
| `font-size: 16px;` (input--lg) | `var(--text-lg)` |
| `font-size: 12px;` | `var(--text-xs)` |
| `padding: 24px;` (card / dialog) | `var(--space-6)` |
| `padding: 16px;` | `var(--space-4)` |
| `padding: 8px 12px;` | `var(--space-2) var(--space-3)` |
| `transition ... 100ms` | `var(--duration-fastest)` |
| `transition ... 150ms` | `var(--duration-fast)` |
| `transition ... 200ms` (switch) | `var(--duration-normal)` |
| `transition ... 300ms` (progress) | `var(--duration-slow)` |
| `gap: 16px / 12px / 8px / 6px / 4px` | matching `--space-*` |

Files touched: `crates/blinc_cn/src/cn_styles.rs`.

Verification: cn_demo screenshot regression. Restrained should
visibly feel tighter (smaller radii, faster transitions) than
Hybrid, and Hybrid tighter than Expressive (bolder radii,
springier durations). All three should render the same as
today's CN_STYLES under `BlincTheme` (whose `radius_default` is
4 not 6 — but BlincTheme isn't the new default fallback, Hybrid
is).

### Phase 3 — Per-widget audit + fix mapping mistakes

After Phase 2, the CN_STYLES rules reference theme tokens but
may pick the **wrong** token. Split into three sub-commits:

#### Phase 3a — Radius mapping

1. **Card / Dialog / Accordion / Toast** — hero surfaces, use
   `var(--radius-xl)` (currently `12px` in CN_STYLES; just
   `var(--radius-md)` would be one step too small).
2. **`.cn-pagination-btn`** — `var(--radius-default)` (matches
   buttons).
3. **`.cn-tooltip`** — `var(--radius-sm)` (was `4px`; Restrained
   wants 3, the rest match accidentally at 4).

#### Phase 3b — Surface tier reshuffle

Apply the four-edit fix from the [Surface section](#surface--background-colour-semantics):

```css
.cn-dialog, .cn-drawer, .cn-sheet, .cn-toast,
.cn-popover-content, .cn-hover-card-content,
.cn-dropdown-menu, .cn-context-menu, .cn-menubar,
.cn-select-content, .cn-combobox-content {
    background: var(--surface-elevated);   /* was --surface */
}

.cn-tabs-list { background: var(--surface-overlay); }       /* was --surface-elevated */
.cn-tabs-trigger--active { background: var(--surface); }     /* was --background */
.cn-accordion { background: var(--surface); }                /* was --surface-elevated */
.cn-accordion-content { background: var(--background); }     /* was --surface */
```

Floating elements lift to elevation 2; tab strip becomes a tonal
tray; accordion outer becomes a card-tier container.

#### Phase 3c — Typography binding (CSS + Rust)

**CSS side** — add a global cn font-family + line-height base
applied across every cn class:

```css
.cn-button, .cn-card, .cn-input, .cn-textarea, ...
   /* see full list in the Typography section above */
{ font-family: var(--font-sans); }

.cn-kbd { font-family: var(--font-mono); }

.cn-alert, .cn-alert-box, .cn-card-content,
.cn-accordion-content, .cn-tooltip {
    line-height: var(--leading-normal);
}

.cn-button, .cn-tabs-trigger, .cn-input, .cn-select-trigger {
    letter-spacing: var(--tracking-tight);
}
```

**Rust side** — replace every `.size(<literal>)` /
`.weight(<literal>)` in cn widget files with reads from
`ThemeState::get().typography()`. Approximate touch points:

| File | Calls to update |
| --- | --- |
| `card.rs` | 4 (title / description / content / footer) |
| `dialog.rs` | 3 (title / description / footer) |
| `alert.rs` | 3 (default / title / description) |
| `button.rs` | 2 (label + icon) |
| `badge.rs` | 1 (label) |
| `tooltip.rs` | 1 |
| `input.rs` / `textarea.rs` | 2 (placeholder + value) |
| `tabs.rs` | 1 (per size variant) |
| `menubar.rs` / `dropdown_menu.rs` / `context_menu.rs` | 1 each (item label) |
| `accordion.rs` | 2 (trigger + content) |
| `breadcrumb.rs` / `pagination.rs` | 1 each |
| `sidebar.rs` / `navigation_menu.rs` | 1 each (item label) |
| `avatar.rs` | 1 (initials) |
| **Total** | ~30 call sites |

Token-to-widget mapping:
- Card title / dialog title → `text_xl` (20) or `text_2xl` (24)
- Section heading / accordion trigger → `text_lg` (17)
- Body text / alert / tooltip / dropdown item → `text_sm` (13)
- Caption / badge / kbd → `text_xs` (12)

#### Phase 3d — Loose ends

1. **`.cn-spinner` rotation accuracy** — separate ticket; the
   timeline binding looks off. Out of scope for the token audit.
2. **`.cn-menubar` "Actions" indicator** — `menubar.rs:338`
   uses `"▶"` ASCII; the demo-side fix uses an actual chevron
   icon. Demo-side change, not theme.
3. **Pagination button hover transition** — `var(--duration-fast)`
   so per-variant durations flow through.

---

## Out of scope (future tickets)

- **Shape tokens in CSS**. The `corner-shape:` CSS property is
  already parsed (`css_parser.rs:4803, 5960, 7241`), but
  `ShapeTokens` aren't exposed as CSS variables — and the
  paint-time auto-apply already handles the common case. Skip
  unless a per-widget shape override becomes necessary.
- **Multi-layer compound shadows** — the Universal variants want
  dual / triple layers but `ShadowTokens` stores one `Shadow`
  per slot. Out of this audit; tracked separately.
- **`font-weight` / `letter-spacing` / `line-height`** — none of
  these are hardcoded in CN_STYLES today, so no audit needed.

---

## Verification plan after Phases 1 + 2 land

Run cn_demo with each Universal variant in turn:

```rust
WindowedApp::run_with_theme(config, RestrainedTheme::bundle().with_css(CN_STYLES), ..., build_ui)
WindowedApp::run_with_theme(config, HybridTheme::bundle().with_css(CN_STYLES), ..., build_ui)
WindowedApp::run_with_theme(config, ExpressiveTheme::bundle().with_css(CN_STYLES), ..., build_ui)
```

Visual diff each section:

| Section | Restrained | Hybrid | Expressive | Expected delta |
| --- | --- | --- | --- | --- |
| Cards (radius) | 14 | 18 | 24 | Expressive most rounded |
| Dialog (radius) | 14 | 18 | 24 | Same — hero surfaces |
| Buttons (radius) | 3 / 6 / 10 | 4 / 8 / 14 | 4 / 10 / 16 | Visibly different per size |
| Body text (size) | 13px (was 14) | 13px | 13px | All tighter than now |
| Body text (font-family) | Noto Sans | Noto Sans | Noto Sans | All three switch from platform default to Noto |
| Body text (line-height) | 1.5 | 1.5 | 1.5 | Multi-line paragraphs open up |
| Dialog bg (Dark mode) | `surface_elevated` lift | same | same | Dialog visibly raised vs surrounding cards |
| Active tab fill | `surface` against `surface_overlay` strip | same | same | Active tab pops forward instead of sinking |
| Hover / press timing | 75-150 ms | 80-180 ms | 100-200 ms | Restrained snappiest, Expressive most springy |
| Button hover ease | quiet ease-out | quiet ease-out | emphasised decel | Expressive feels "Material-y" on press |

All three should now feel distinct. Today they feel identical
because CN_STYLES locks geometry / motion / font-family / surface
semantics to single values.
