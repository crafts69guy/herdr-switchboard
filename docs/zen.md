# Zen mode

Zen gives one pane the screen without restarting its process. `switchboard.zen-toggle` acts on the
current pane without opening a picker; `switchboard.zen` lets you choose another pane.

The pane moves into a tab of its own and occupies `zen.width` percent of the tab. The remaining
space becomes two gutters. With `zen.scrim = true`, Switchboard paints those gutters using Herdr's
graphics API and `zen.scrim_color`.

```toml
[zen]
width = 70
scrim = true
scrim_color = "#11111b"
chrome = "off"
```

Scrims require this setting in Herdr's own config:

```toml
[experimental]
kitty_graphics = true
```

Without it, the gutters remain blank and Zen continues to work.

## Restoration limits

Only the focused pane moves. Neighbouring panes continue running in the original tab. Leaving Zen
restores a two-pane tab exactly and restores panes beside their former neighbour in deeper layouts.

Herdr 0.8.0 cannot insert a pane at an arbitrary point in a nested split tree. For a deeply nested
layout, every pane returns but the nesting can differ. Switchboard reports that case rather than
silently implying an exact restore.

## Hiding Herdr chrome

`zen.chrome` can temporarily hide global Herdr UI elements:

| Value | Herdr `[ui]` keys changed during Zen |
| --- | --- |
| `off` | Nothing; this is the default. |
| `panes` | `pane_borders`, `pane_gaps`, and `pane_scrollbars`. |
| `full` | The pane keys plus the single-tab tab bar and sidebar settings. |

These settings are global, so `panes` and `full` affect every workspace while the Zen session is
active. Switchboard edits Herdr's config with `toml_edit`, preserving comments and unmanaged keys,
then restores the exact previous values on exit.

Before the first change it copies the untouched config to:

```text
$XDG_STATE_HOME/herdr-switchboard/herdr-config.backup.toml
```

If restoration fails or Herdr is killed mid-session, retry it with:

```sh
herdr-switchboard zen chrome-restore
```

Herdr applies `sidebar_start_collapsed` only on its next launch. `chrome = "full"` may therefore
leave the sidebar visible in the current process; Switchboard detects and reports that case.

Zen does not use `herdr pane zoom`: splitting a zoomed tab cancels Herdr's zoom, which conflicts
with the gutter layout. Use Herdr's own zoom when you want plain fullscreen without centring.
