# Writing widgets

A widget is a single `.nitroget` file with three blocks: a template, a
stylesheet and a Lua script. Lua globals are the widget's state; timers and
event handlers update them, and Nitroget writes only the UI properties that
changed. Placement and monitor selection are machine-local and never part of the
widget.

## Contents

- [A minimal widget](#a-minimal-widget)
- [The `<widget>` root](#the-widget-root)
- [Template](#template)
  - [Elements](#elements)
  - [Control flow](#control-flow)
  - [Expressions and escaping](#expressions-and-escaping)
- [Styles](#styles)
  - [Following the GTK theme](#following-the-gtk-theme)
- [Script](#script)
  - [API](#api)
- [Reloading](#reloading)
- [Validation](#validation)
- [Limitations](#limitations)

## A minimal widget

```html
<widget name="clock" width="320" height="150">
  <box class="card">
    <label class="time">{time}</label>
  </box>
</widget>

<style>
  .card { background-color: #111; border-radius: 16px; padding: 16px; }
  .time { font-size: 40px; color: white; }
</style>

<script>
  time = os.date("%H:%M")
  nitroget.every(1000, function() time = os.date("%H:%M") end)
</script>
```

The three blocks are:

| Block | Required | Purpose |
| --- | --- | --- |
| `<widget>` | yes | structure, plus intrinsic size and layer |
| `<style>` | no | GTK CSS, scoped to this widget |
| `<script>` | no | Lua, runs once when the widget loads |

## The `<widget>` root

| Attribute | Default | Meaning |
| --- | --- | --- |
| `name` | file stem | CSS scope and the key in `config.toml` |
| `width` | `320` | surface width in logical pixels |
| `height` | `160` | surface height in logical pixels |
| `layer` | `bottom` | `bottom`, `top`, `overlay`, or `background` |

`bottom` puts the widget above the wallpaper and below normal windows, which is
what desktop widgets want. `background` is reserved for wallpaper daemons and
usually receives no input.

Position and monitor are **not** part of the widget. They live in
`config.toml` next to the widgets (for the default folder,
`~/.config/nitroget/config.toml`; a custom `--widgets` folder gets its own).
See [`config.example.toml`](config.example.toml) and `nitroget placement`.
`config.toml` must be a regular file; symlinks are refused.
Writing `anchor`, `offset` or `output` in a widget is ignored, and
`nitroget check` tells you so.

`name` must be unique within a widget folder: it scopes the CSS and is the key
the placement config uses.

## Template

### Elements

| Element | What it is |
| --- | --- |
| `box` | vertical container |
| `row` | horizontal container |
| `label` | text |
| `bar` | progress bar |
| `slider` | horizontal slider |
| `button` | clickable button |
| `image` | picture from a file |
| `spacer` | flexible empty space |
| `sep` | horizontal separator |

### Common attributes

These work on every element:

| Attribute | Meaning |
| --- | --- |
| `class` | CSS classes, space-separated; may contain `{ expr }` |
| `id` | GTK widget name |
| `width`, `height` | fixed size request |
| `expand` | expand in both directions |
| `hexpand`, `vexpand` | expand in one direction |
| `halign`, `valign` | `start`, `center`, `end`, `fill` |
| `tooltip` | tooltip text; may contain `{ expr }` |
| `visible` | show/hide; a Lua expression such as `{count > 0}` |
| `on:click` | Lua code to run when clicked |

`on:click` is passed to Lua as a chunk. Either call a function or write the
statement directly:

```html
<button on:click="increment()">+1</button>
<button on:click="count = count + 1">+1</button>
```

On a `button` the handler runs on click. On other elements it runs on pointer
press.

### `box` and `row`

Vertical and horizontal containers. `gap` is the spacing in pixels.

```html
<row gap="8">
  <label>left</label>
  <spacer />
  <label>right</label>
</row>
```

### `label`

Text comes from the `text` attribute or from the element's children.
Interpolation works in both.

```html
<label text="CPU: {cpu}%" />
<label class="time">{time}</label>
```

| Attribute | Meaning |
| --- | --- |
| `text` | text template (instead of children) |
| `wrap` | `true` to wrap long text |
| `align` | `start`, `center`, `end` |

### `bar`

Progress bar. `value` and `max` are numbers; both may be expressions.

```html
<bar value="{cpu}" max="100" />
```

The fraction is `value / max`, clamped to 0–1. It is styled through its tag
class:

```css
bar trough   { min-height: 7px; border-radius: 4px; background-image: none; background-color: rgba(255,255,255,0.1); }
bar progress { min-height: 7px; border-radius: 4px; background-image: none; background-color: #34d399; }
```

GTK themes often paint progress bars with a gradient, so `background-image:
none` is usually needed before `background-color` takes effect.

### `slider`

Horizontal `GtkScale`.

```html
<slider value="{volume}" min="0" max="100" step="1" on:change="set_volume()" />
```

| Attribute | Default | Meaning |
| --- | --- | --- |
| `value` | `0` | current value; may be an expression |
| `min` | `0` | minimum |
| `max` | `100` | maximum |
| `step` | `1` | step size |
| `on:change` | | Lua code run while dragging |

The triggering value is available in the global `event` table:

```lua
function set_volume()
  volume = math.floor(event.value)
end
```

Styling uses the same GTK nodes as `GtkScale`:

```css
slider trough    { min-height: 6px; background-image: none; background-color: rgba(255,255,255,0.12); }
slider highlight { min-height: 6px; background-image: none; background-color: #6366f1; }
slider slider    { min-width: 14px; min-height: 14px; background-image: none; background-color: white; }
```

### `button`

A `text` attribute is optional; text children work too, and the button can
contain other elements.

```html
<button on:click="reset()">reset</button>
<button on:click="toggle()"><label>{play_icon}</label></button>
```

### `image`

Loads a picture from a file. `~/` is expanded. Images are decoded once and
cached, so updating other parts of the widget does not reload the file.

```html
<image src="~/Pictures/avatar.png" width="64" height="64" />
<image src="{cover}" width="96" height="96" fit="cover" />
```

| Attribute | Default | Meaning |
| --- | --- | --- |
| `src` | | file path; may contain `{ expr }` |
| `fit` | `contain` | `contain`, `cover`, `fill` |

### `spacer` and `sep`

`spacer` expands to fill available space. Add `vertical="true"` to also expand
vertically. `sep` is a horizontal separator line.

### Control flow

```html
{#if count >= 10}
  <label class="warn">that is a lot of clicks</label>
{:else}
  <label>keep going</label>
{/if}
```

```html
{#each disks as disk}
  <label>{disk.name}: {disk.used}%</label>
{/each}

{#each disks as disk, i}
  <label>#{i} {disk.name}</label>
{/each}
```

`{:else if}` is not supported; nest another `{#if}` instead. Lists can be Lua
arrays or keyed tables. Rows are updated in place while the number of items is
unchanged; adding or removing items rebuilds that list.

### Expressions and escaping

Any `{ expression }` is evaluated by Lua:

```html
<label>{count + 1}</label>
<label>{os.date("%H:%M")}</label>
<label>{#items}</label>
```

In a numeric attribute the value is used as a number:

```html
<bar value="{cpu}" max="{limit}" />
```

Write a literal brace as `\{`.

## Styles

The `<style>` block is standard GTK CSS. Every selector is automatically scoped
to the widget, so `.card` cannot affect another widget.

Element tags are also exposed as classes, which makes them easy to style:

```css
.card { background-color: rgba(18, 18, 24, 0.72); border-radius: 18px; padding: 18px; }
box    { /* any box */ }
label  { /* any label */ }
bar trough { /* progress bar track */ }
```

`:host` and `widget` refer to the widget root.

### Following the GTK theme

Standard controls such as `button`, `label` and `slider` already use the current
GTK theme. For custom surfaces, use the theme's named colors instead of fixed
values:

```css
.card {
  background-color: alpha(@theme_bg_color, 0.82);
  color: @theme_fg_color;
  border: 1px solid @borders;
}

.accent {
  background-color: @theme_selected_bg_color;
  color: @theme_selected_fg_color;
}
```

`@theme_bg_color`, `@theme_fg_color`, `@theme_base_color`, `@theme_text_color`,
`@theme_selected_bg_color`, `@theme_selected_fg_color` and `@borders` are the
portable names. Themes based on libadwaita also provide `@window_bg_color`,
`@view_bg_color`, `@card_bg_color` and `@accent_bg_color`. GTK re-resolves these
when the theme changes; hard-coded colors do not follow it. Omit `font-family`
to inherit the theme font.

The outer window is transparent by design, so apply the background to the card
or another content element. Undefined color names are ignored silently by GTK.

This is GTK CSS, not browser CSS. Colors, backgrounds, borders, border radii,
padding, margins, fonts, shadows, gradients and transitions work; layout
properties such as `display: flex` do not. Layout comes from `box`/`row`.

## Script

The script runs once when the widget loads, in Lua 5.4. Anything it assigns to a
global is visible to the template:

```lua
time = os.date("%H:%M")
```

Use globals, not `local`, for values the template reads. `local` is fine for
helpers and private state.

`io`, `os` and shell commands are available, because widgets are local code you
chose to run. Treat downloaded widgets like any script.

### API

| Call | Description |
| --- | --- |
| `nitroget.every(ms, fn)` | run `fn` every `ms`; returns an id |
| `nitroget.after(ms, fn)` | run `fn` once after `ms` |
| `nitroget.clear(id)` | cancel a timer |
| `nitroget.exec(cmd, fn)` | run a shell command off-thread; `fn(ok, output)` |
| `nitroget.run(cmd)` | run a shell command and ignore the output |
| `nitroget.refresh()` | force an update (timers and handlers do this already) |
| `nitroget.log(msg)` | print to the terminal |
| `nitroget.quit()` | stop the daemon |
| `nitroget.version` | engine version string |

### Updating data

```lua
local function read_cpu()
  local file = io.open("/proc/stat")
  local line = file:read("*l")
  file:close()
  -- ... parse and compute ...
  cpu = percent
end

nitroget.every(2000, read_cpu)
```

After a timer, click, slider change or `exec` callback returns, Nitroget
updates the widget. The script itself is **not** re-run; only the values that
changed are written to GTK.

### Running commands

```lua
-- asynchronous, with a result
nitroget.exec("playerctl metadata --format '{{title}}'", function(ok, output)
  if ok then track = output end
end)

-- fire and forget
function next_track()
  nitroget.run("playerctl next")
end
```

`exec` runs through `sh -c` on a worker thread, so a slow command does not
freeze the desktop.

## Running

Only one daemon runs at a time. It uses the widget folder it was started with
and keeps its placement in that folder's `config.toml`. `nitroget placement` and
`nitroget quit` find the running instance automatically; passing `-w DIR`
checks that it is the folder in use. To switch folders, run `nitroget quit`,
then start with the new folder.

## Reloading

Saving a widget file reloads it after about 400 ms: the Lua script runs again,
timers restart and Lua state is reset.

Changing the anchor/offset in `config.toml` or `nitroget placement` moves the
window without reloading the script, so widget state survives a move. Changing
monitor, layer or size builds a new surface and reloads the widget.

## Validation

```sh
nitroget check --widgets ~/.config/nitroget
```

This parses the template, compiles the Lua, applies the config and prints the
effective placement of each widget. Errors and ignored attributes are reported
with the file name.

## Limitations

- The element vocabulary is fixed; there is no arbitrary HTML or canvas.
- `{#each}` is index-based. Reordering items with the same count updates rows by
  position rather than moving widgets.
- Every expression is re-evaluated on each update, though only changed
  properties are written to GTK.
- Widgets on the `bottom` layer get pointer input but usually not keyboard
  focus. Text input and overlays are not supported yet.
- GNOME/Mutter does not implement layer-shell; it is not supported.
- Widgets run with your user privileges and can execute commands.
