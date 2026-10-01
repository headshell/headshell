# Writing a theme

A theme changes `headshell`'s colours, corner radii and animation duration. It
runs no JavaScript, accesses no data and doesn't change the app's behaviour —
appearance only.

The two themes in this directory (`daylight`, `contrast`) ship with the app and
are examples at the same time: copy one and write your own.

## Installing

A theme is a directory with two files in it:

```
<data-directory>/themes/<theme-name>/
  theme.json
  theme.css
```

The data directory's path is written in the app's **appearance** section
(usually `~/.local/share/headshell`). Put the directory there and say
**refresh the list** — you don't need to close the app. The directory name is
the theme's ID.

In the list, every theme shows up as a small window drawn in its own colours:
the `--headshell-bg`, `--headshell-surface`, `--headshell-text` and
`--headshell-accent` you wrote in `:root`, and for the ones you didn't write,
the default theme's — exactly what you'll see when it's applied. Conditional
values inside `@media` don't go into the preview.

`theme.json`:

```json
{
  "name": "My Theme",
  "author": "You",
  "api": 1
}
```

`api` is the version of the contract below. On a mismatch the theme **isn't
silently ignored**; it shows up in the list together with the version it asks
for.

## The contract: tokens

The guaranteed surface is **only** these variables in the `:root` block of
`theme.css`. A token you don't write stays at its default value.

| Token | Default | Meaning |
|---|---|---|
| `--headshell-color-scheme` | `dark` | `dark` \| `light` — for the parts the engine draws itself, like checkboxes, the caret and the scrollbar |
| `--headshell-bg` | `#12100e` | page background |
| `--headshell-surface` | `#1b1815` | panel background (top bar, sidebar, player) |
| `--headshell-surface-raised` | `#221e1a` | interactive/hovered background (input, active tab, notice) |
| `--headshell-border` | `#2e2925` | border, divider, progress bar track |
| `--headshell-text` | `#eae2d8` | primary text |
| `--headshell-text-dim` | `#9a8f83` | secondary/dim text |
| `--headshell-accent` | `#ffb454` | brand + interaction accent |
| `--headshell-success` | `#7bd88f` | playing / success |
| `--headshell-error` | `#ff6b6b` | error |
| `--headshell-info` | `#79c0ff` | buffering / info |
| `--headshell-radius-sm` | `6px` | control corner radius |
| `--headshell-radius-lg` | `8px` | panel corner radius |
| `--headshell-duration` | `120ms` | animation duration (`0ms` = no motion) |

The shortest working theme:

```css
:root {
  --headshell-accent: #7aa2f7;
}
```

## Going outside `:root`

Class names (`.topbar`, `.player`, `.queue`, `.toast`, …) are part of the
contract too, and they aren't renamed without notice — the full list is the
`CONTRACT_CLASSES` array in `crates/headshell/tests/ui_contract.rs`, and a test
holds it. But a theme that writes outside `:root` is flagged in the list as
**"extended · no guarantee"**.

It isn't rejected — we don't block it, and we don't hide it either. Across an
`api` version bump, backward compatibility is committed only for the tokens
above.

Class names staying in place doesn't mean **the layout** will stay the same.
The skeleton changed in D-072: the sidebar became full height, `.topbar` moved
above the content column and `.player` below it, and `.state` became a dot
instead of a glyph (its colour still changes with
`.state.playing { color: … }`). No class was removed or changed its meaning —
it stayed at `api` 1 — but an extended theme that assumes positions may feel
it.

D-075 changed it once more: "now playing" is no longer a section inside
`.content` but a sheet over it that rises out of `.player`. `.main` became a
two-row grid (`.topbar`, then a row that `.content`, a scrim and the sheet
share), and `#panel-now` still carries `.panel` but sits outside `.content`.
The sheet's position is written by the springs as an inline `transform`; a
theme that gives it one of its own breaks it.

D-076 added covers. When the playing track has one, it is the record's label:
`.record-cover`, inside the record's drawing so it turns with it, faded in by
the springs as an inline `opacity` — a theme that gives it an `opacity` of its
own breaks the fade. Every queue row got a square slot, `.q-cover`, whose
background (a small record) stands in until a cover arrives or when there is
none, and `.cover-note` under the queue's title says what could not be looked
up. They are new classes, not in `CONTRACT_CLASSES`: styling them is
extending.

D-079 added context menus: a single `.menu` over everything, filled with
`.menu-title`, `.menu-item` rows (`.active` when lit, `.danger`, `.armed`
while it asks "really …?", `aria-disabled="true"`) holding `.menu-check`,
`.menu-label` and `.menu-key`, and `.menu-separator`; `.menu-target` rings
the element a menu is about while it is open. The menu is placed and grown out
of the pointer by the springs: its `left`, `top`, `transform-origin`,
`transform` and `opacity` are written inline, so a theme that gives `.menu` a
`transform` breaks the growing. New classes too, not in `CONTRACT_CLASSES`.

D-080 added two more. The play box's suggestions are `.suggest` (under the
field, its `transform`, `opacity` and width written inline — the same warning
as `.menu`), with `.suggest-row` (`.lit` when lit, `.artist` for an artist),
`.suggest-art` (a found cover is an `img` inside it), `.suggest-main`, `.suggest-title`, `.suggest-sub`,
`.suggest-time`, `.suggest-heading` ("results from …"), `.suggest-note` and
`.suggest-failure`. The providers section's choice is `.play-from` (a
`.card`), `.play-from-list`, `.play-from-row` (`aria-checked="true"` when
chosen), `.play-from-name`, `.play-from-id` and `.play-from-pill`, whose
`transform` the springs write. New classes, not in `CONTRACT_CLASSES`.

## Animation: `transform` and `opacity` only

This isn't a matter of style; it's a measurement. In WebKitGTK (the default
engine on Linux), `height`, `box-shadow`, `filter` and `background-position`
animations drop the frame rate from 58.8 to 47.6; `transform` and `opacity`
don't drop it at all.

That's why the progress bar is drawn with `scaleX`, not with width. Do the
same — otherwise the difference shows up on **the user's** machine, and the one
blamed is the app, not the theme.

### `--headshell-duration` scales all motion

The interface's motion comes in two kinds: short transitions (hover, the
drag-and-drop area) use this duration directly; positional motion (the
selection indicator, section transitions, notices, the shortcut window) is a
**spring**, and the spring's response is three times this duration — the
default 120ms → 0.36 s.

- `0ms` means nothing moves: the springs stop, and so does the spinning busy
  ring. The High Contrast theme uses this.
- A longer duration slows all motion down proportionally.
- The system's "reduce motion" preference applies independently of the theme:
  positional and scale motion goes away, opacity transitions stay.

## Versioning

- **Adding** a new token doesn't raise `api`: your theme didn't write it, so it
  gets the default and keeps working.
- **Removing** a token or changing its meaning raises it.

The current version: **`api: 1`**.
