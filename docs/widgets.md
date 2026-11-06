# Writing widgets

A widget is a single `.nitroget` file with three blocks: a template, a
stylesheet and a Lua script. Lua globals are the widget's state; timers and
event handlers update them, and Nitroget writes only the UI properties that
changed. Placement and monitor selection are machine-local and never part of the
widget. A `.nitroget` file can also define an edge-attached panel with
`<panel>`.

## Contents

- [A minimal widget](#a-minimal-widget)
- [The `<widget>` root](#the-widget-root)
- [The `<panel>` root](#the-panel-root)
- [Template](#template)
  - [Elements](#elements)
  - [Control flow](#control-flow)
  - [Expressions and escaping](#expressions-and-escaping)
- [Styles](#styles)
  - [Following the GTK theme](#following-the-gtk-theme)
- [Script](#script)
  - [External scripts](#external-scripts)
  - [API](#api)
  - [Shared and per-output state](#shared-and-per-output-state)
  - [Per-output timers](#per-output-timers)
  - [JSON](#json)
  - [Streaming output](#streaming-output)
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
| `<widget>` or `<panel>` | yes | root surface; `<panel>` is edge-attached |
| `<style>` | no | GTK CSS, scoped to this surface |
| `<script>` | no | Lua, inline or loaded with `src`; runs once when the surface loads |

## The `<widget>` root

| Attribute | Default | Meaning |
| --- | --- | --- |
| `name` | file stem | CSS scope and the key in `config.toml` |
| `width` | auto | surface width in logical pixels; omitted, GTK uses the natural size |
| `height` | auto | surface height in logical pixels; omitted, GTK uses the natural size |
| `layer` | `bottom` | `bottom`, `top`, `overlay`, or `background` |

`bottom` puts the widget above the wallpaper and below normal windows, which is
what desktop widgets want. `background` is reserved for wallpaper daemons and
usually receives no input.

A template with a single top-level element expands that element to fill the
surface, so its visible background matches the surface size.

If `width` or `height` is omitted, GTK sizes that axis to the widget's natural
minimum content size. `nitroget placement` uses the measured size, so outlines
match auto-sized widgets too. `config.toml` can still override either axis.

Position and monitor are **not** part of the widget. They live in
`config.toml` next to the widgets (for the default folder,
`~/.config/nitroget/config.toml`; a custom `--widgets` folder gets its own).
See [`config.example.toml`](config.example.toml) and `nitroget placement`.
`config.toml` must be a regular file; symlinks are refused.
Writing `anchor`, `offset` or `output` in a widget is ignored, and
`nitroget check` tells you so.

`defaults.renderer` selects the GTK renderer: `cairo` (default), `auto`, `gl` or
`vulkan`. Cairo uses the least memory; `auto` lets GTK choose a GPU renderer.
An existing `GSK_RENDERER` environment variable takes precedence.

`name` must be unique within a widget folder: it scopes the CSS and is the key
the placement config uses.

## The `<panel>` root

A `<panel>` is an edge-attached surface for bars and panels. It uses the same
template, style and script blocks as a widget, but its position comes from the
file rather than `config.toml`.

```html
<panel name="bar" edge="top" width="full" height="50" layer="top">
  <row class="bar" gap="12">
    <label>left</label>
    <spacer />
    <label>right</label>
  </row>
</panel>
```

| Attribute | Default | Meaning |
| --- | --- | --- |
| `name` | file stem | CSS scope and the key in `config.toml` |
| `edge` | required | `top`, `bottom`, `left`, or `right` |
| `offset` | `0` | gap between the output edge and the panel |
| `layer` | `bottom` | `bottom`, `top`, `overlay`, or `background` |
| `width` | | thickness for left/right panels; `full` or omitted for top/bottom |
| `height` | | thickness for top/bottom panels; `full` or omitted for left/right |

Top and bottom panels span the full output width. Left and right panels span the
full output height. A panel keeps its configured thickness; content larger than
the panel is clipped.

Panels are not part of the machine-local placement system. `config.toml` only
applies `enabled` and `output` to a panel; `anchor`, `offset`, `width`, `height`,
`layer` and per-monitor `outputs` overrides in `[widgets.<name>]` are ignored.
`nitroget placement` does not show panels.

A panel reserves an exclusive zone automatically. The zone is
`offset + thickness` along its edge, so other windows avoid it. The zone is an
edge strip only; a panel cannot reserve an arbitrary rectangle in the middle of
an output.

`output = "all"` creates one panel per monitor. All copies share one Lua state
and timer set; use `output` or `nitroget.outputs` for per-monitor state.

```toml
[widgets.bar]
output = "all"
```

`nitroget check` reports the panel edge, thickness and computed exclusive zone.

## Template

### Elements

| Element | What it is |
| --- | --- |
| `box` | vertical container |
| `row` | horizontal container |
| `grid` | equal-width rows and columns |
| `scroll` | vertically scrollable container |
| `label` | text |
| `bar` | progress bar |
| `slider` | horizontal slider |
| `entry` | single-line text input |
| `select` | dropdown selection |
| `button` | clickable button |
| `image` | picture from a file |
| `spacer` | flexible empty space |
| `sep` | horizontal separator |

### Attribute values

A bare attribute value is literal. A `{ ... }` value is always a Lua
expression:

```html
<label text="Clear" />
<label text="{draft}" />
<label text="CPU: {cpu}%" />

<label visible="true" />
<label visible="{count > 0}" />

<select options="{models}" selected="{model}" />
<select options="{models}" selected="qwen3.5:4b" />
```

For string attributes (`class`, `text`, `tooltip`, `src`, `placeholder`),
literal text and expression segments may be mixed. For typed attributes
(boolean, number, enum, and table), the whole value must be either a valid
literal or one expression. `options` requires an expression returning a table.
`on:click`, `on:change` and `on:submit` contain raw Lua code.

Static flags (`markdown`, `selectable`, `wrap`, `follow`, `vertical`, `expand`,
`hexpand`, `vexpand`) accept only `true` or `false`; they do not accept
expressions. Layout attributes such as `width`, `height`, `gap` and `columns`
are evaluated when the widget is built. `class`, `text`, `tooltip`, `src`,
`visible`, bar/slider `value` and `max`, `options`, and `selected` are updated
on each render.

Write a literal brace as `\{`.

### Common attributes

These work on every element:

| Attribute | Meaning |
| --- | --- |
| `class` | CSS classes, space-separated; template |
| `id` | GTK widget name; literal or expression |
| `width`, `height` | fixed size request; number or expression |
| `expand` | `true` or `false` |
| `hexpand`, `vexpand` | `true` or `false` |
| `halign`, `valign` | `start`, `center`, `end`, `fill`; literal or expression |
| `tooltip` | tooltip text; template |
| `cursor` | cursor name; literal or expression |
| `visible` | `true`, `false`, or a boolean expression |
| `on:click` | Lua code to run when clicked |

`on:click` is passed to Lua as a chunk. Either call a function or write the
statement directly:

```html
<button on:click="increment()">+1</button>
<button on:click="count = count + 1">+1</button>
```

Inside `{#each}`, `event.index` is the one-based row index and `event.button`
is the pointer button.

### `box` and `row`

Vertical and horizontal containers. `gap` is the spacing in pixels.

```html
<row gap="8">
  <label>left</label>
  <spacer />
  <label>right</label>
</row>
```

### `grid`

Lays direct children out in rows and columns. `columns` defaults to `1`; columns
are homogeneous, so every cell has the same width and one- and two-digit values
remain aligned. `gap` adds spacing between cells.

```html
<grid columns="7">
  <label>Mo</label>
  <label>Tu</label>
  <!-- ... -->
</grid>
```

### `scroll`

A vertically scrollable container. Put its content in a single child container.
Set `max-height` to let it grow with its content up to that height; content
beyond it scrolls.

```html
<scroll class="transcript" max-height="340" follow="true">
  <box gap="8">
    {#each messages as message}
      <label wrap="true">{message.content}</label>
    {/each}
  </box>
</scroll>
```

| Attribute | Default | Meaning |
| --- | --- | --- |
| `max-height` | | maximum natural height in pixels |
| `follow` | `false` | stay at the bottom while content grows |

`follow` stops following when the user scrolls up, and resumes when the user
returns to the bottom. Horizontal scrolling is disabled.

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
| `markdown` | `true` or `false`; renders CommonMark/GFM as Pango markup |
| `selectable` | `true` or `false`; allows selecting and copying the text |
| `wrap` | `true` or `false`; wraps long text |
| `align` | `start`, `center`, `end`; literal or expression |

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

### `entry`

Single-line text input (`GtkEntry`).

```html
<entry text="{draft}" placeholder="Message" on:change="draft = event.value" on:submit="send(event.value)" />
```

| Attribute | Meaning |
| --- | --- |
| `text` | current text; may contain `{ expr }` |
| `placeholder` | placeholder shown when empty |
| `on:change` | Lua code run whenever the text changes |
| `on:submit` | Lua code run when Enter is pressed |

`event.value` is the current text as a string. Programmatic changes to `text`
do not fire `on:change`. Keyboard focus is requested on demand while a visible
`entry` exists.

### `select`

Dropdown selection (`GtkDropDown`).

```html
<select options="{models}" selected="{model}" on:change="model = event.value" />
```

| Attribute | Meaning |
| --- | --- |
| `options` | expression returning an array of strings |
| `selected` | literal string or expression returning the selected string |
| `on:change` | Lua code run when the selection changes |

`event.value` is the selected string. If `selected` is missing from `options`,
the first option is used. Changing `options` or `selected` updates the dropdown
without firing `on:change`. A dropdown with no options is insensitive.

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
| `src` | | file path template |
| `fit` | `contain` | `contain`, `cover`, `fill`; literal or expression |

When both `width` and `height` are set, the image is rendered in exactly that
box, centered inside its parent, and scaled according to `fit`. With only one
dimension, the other follows the image’s natural size.

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
unchanged; adding or removing items rebuilds that list. The loop index is
one-based. A `{#each}` block follows the orientation of its parent `box` or
`row`.

### Expressions and escaping

Any `{ expression }` is evaluated by Lua:

```html
<label>{count + 1}</label>
<label>{os.date("%H:%M")}</label>
<label>{#items}</label>
```

Bare values are literal. Quotes delimit an attribute value; they do not make it
a string:

```html
<label visible="{count > 0}" />
<label text="Clear" />
<label text="{draft}" />
<label text="CPU: {cpu}%" />
<bar value="{cpu}" max="100" />
```

For a typed attribute, the whole value must be a literal or one expression.
Invalid values are errors rather than silent defaults.

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

### External and shared stylesheets

Use `<style src="...">` to load CSS from a file:

```html
<style src="../theme.css" />

<style>
  .card {
    padding: 10px;
  }
</style>
```

Paths are relative to the widget file and may point outside the widget folder
for a shared theme. External CSS is read at load and scoped exactly like inline
CSS. Multiple style blocks are concatenated in source order, so a shared theme
can precede or follow widget-specific rules.

`<style src>` cannot also contain inline CSS. Symlinked stylesheets are
rejected, and stylesheet files under the widget root are watched for reloads.
Use `<style src>` instead of `@import`; `@import` rules are not scoped by the
engine.

`@define-color` at-rules remain global to the display. Use a unique prefix for
shared palette names to avoid collisions between themes.

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

The script runs once for the widget path when the surface loads, in Lua 5.4.
All monitor copies created by `output = "all"` share that Lua state. Anything it
assigns to a global is shared by every copy and visible to the template:

```lua
time = os.date("%H:%M")
```

Use globals, not `local`, for values the template reads. `local` is fine for
helpers and private state.

`io`, `os` and shell commands are available, because widgets are local code you
chose to run. Treat downloaded widgets like any script.

### External scripts

Keep a long script in its own file:

```html
<script src="widget.lua"></script>
```

`src` is resolved relative to the `.nitroget` file and must stay inside the
widget folder. The file runs once for the widget path when the surface loads,
like an inline script.

Scripts can use `require` for other Lua files:

```lua
local helpers = require("lib.helpers")
local ui = require("ui")
```

`require("lib.helpers")` loads `lib/helpers.lua`, while `require("ui")` loads
`ui.lua` or `ui/init.lua`. Modules are searched in the widget folder and in the
folder containing the entry script, so nested modules work. Editing a `.lua`
file in the widget folder reloads the widget.

### API

| Call | Description |
| --- | --- |
| `nitroget.every(ms, fn)` | run `fn` every `ms`, shared by the whole widget; returns an id |
| `nitroget.after(ms, fn)` | run `fn` once after `ms` for the whole widget |
| `nitroget.every_output(ms, fn)` | run `fn(output)` every `ms` for each active output |
| `nitroget.after_output(ms, fn)` | run `fn(output)` once after `ms` for each active output |
| `nitroget.clear(id)` | cancel a timer or stream |
| `nitroget.exec(cmd, fn)` | run a shell command off-thread; `fn(ok, text)` |
| `nitroget.stream(cmd, stdin, on_line, on_done)` | run a shell command off-thread and stream its stdout |
| `nitroget.json_encode(value)` | convert a Lua value to JSON text |
| `nitroget.json_decode(text)` | convert JSON text to a Lua value |
| `nitroget.run(cmd)` | run a shell command and ignore the output |
| `nitroget.menu(items, options)` | open a menu |
| `nitroget.close_menu()` | close the currently open menu |
| `nitroget.update_menu(items)` | replace the items of the open menu |
| `nitroget.refresh()` | force an update (timers and handlers do this already) |
| `nitroget.log(msg)` | print to the terminal |
| `nitroget.quit()` | stop the daemon |
| `nitroget.version` | engine version string |
| `nitroget.dir` | path to the widget folder |

### Menus

`nitroget.menu(items, options)` opens a menu. It can be called from an
`on:click` handler or from a timer; a click anchors the menu at the click, while
a timer anchors it at the bottom center of the widget. `x` and `y` override the
position. Only one menu is open at a time.

```lua
nitroget.menu({
  { id = "open", label = "Open" },
  { separator = true },
  { id = "mute", label = "Mute", type = "check", checked = muted },
  { label = "Output", submenu = {
      { id = "speakers", label = "Speakers" },
      { id = "headphones", label = "Headphones" },
  } },
}, {
  on_select = function(id, checked)
    if id == "open" then
      open()
    elseif id == "mute" then
      muted = checked
    end
  end,
})
```

For a slider popup:

```lua
nitroget.menu({
  { type = "label", label = "Brightness" },
  { separator = true },
  { type = "slider", min = 1, max = 100, step = 1, value = brightness, show_value = false, action = "set_brightness(event.value)" },
}, { y = 50 })
```

| Field | Meaning |
| --- | --- |
| `id` | value passed to `on_select`; keeps its Lua type |
| `label` | menu text |
| `action` | Lua code run when selected; alternative to `id` |
| `submenu` | nested item array |
| `enabled` | `false` disables the item |
| `type` | `check`, `radio`, `slider` or `label` |
| `checked` | current check/radio state |
| `value`, `min`, `max`, `step` | slider value and bounds |
| `show_value` | show the slider value; default `true` |
| `selected` | highlights the item as the current choice |
| `separator` | draws a section separator |
| `icon` | icon name or absolute file path |

| Option | Meaning |
| --- | --- |
| `on_select` | callback `(id, checked)` for items with an `id` |
| `x`, `y` | position relative to the widget root |
| `output` | connector to anchor the menu on; useful when a widget spans outputs |
| `layout` | `menu` (default), `row` or `column` |
| `close_on_select` | close after selection; defaults to `true` for `menu` and `false` for flat layouts |

Numeric ids stay numbers; string ids stay strings. Use `selected = true` for
the current choice and `enabled = false` to keep it visible but non-interactive.
`checked` only controls check and radio markers.

`type = "slider"` renders a horizontal scale. Set `min`, `max`, `step` and
`value`; `show_value = false` hides the numeric readout. Its `action` runs while
dragging with `event.value` available.

`type = "label"` renders a non-interactive heading.

`layout = "menu"` is vertical and supports submenus, separators, icons, and
check/radio markers. `layout = "row"` and `layout = "column"` are flat button
lists; `row` is horizontal, `column` is vertical. Flat layouts do not render
submenus, and `row` does not render separators.

`close_on_select` defaults to `true` for `menu` and `false` for `row` and
`column`.

`nitroget.update_menu(items)` replaces the items of the open menu.
`nitroget.close_menu()` closes it. On a multi-output widget, `output` selects
the output used by a timer-opened menu.

### Shared and per-output state

Normal Lua globals are shared by every monitor copy of a widget. Use them for
values that should be the same everywhere:

```lua
clock = os.date("%H:%M")

nitroget.every(1000, function()
  clock = os.date("%H:%M")
end)
```

Each active output has its own table in `nitroget.outputs`, keyed by connector.
The same table is available as the global `output` while that output is
rendering or handling an event:

```lua
nitroget.every_output(2000, function(output)
  output.cpu = read_cpu()
end)
```

```html
<label>{clock}</label>
<label>{output.connector}: {output.cpu}%</label>
```

Each output table contains `connector`, `index`, `width` and `height`, plus any
fields the script assigns. Globals are shared; `output.<field>` belongs to one
monitor. The global `output` name is reserved by the engine; use
`nitroget.outputs` to iterate every active output.

For per-monitor copies, `index` identifies the monitor. A single-output widget
may have a nil `index`. When the compositor chooses the output, `connector` can
be empty and the table is keyed by a synthetic index.

### Per-output timers

`nitroget.every` and `nitroget.after` create one timer for the whole widget.
The callback runs once, with `output` set to nil.

`nitroget.every_output` and `nitroget.after_output` invoke the callback for every
active output on the same interval. The callback receives that output table, and
the global `output` refers to the same table.

```lua
nitroget.every_output(1000, function(output)
  output.time = os.date("%H:%M")
end)
```

Per-output timers are useful when each monitor needs its own command or value.
Global timers are cheaper when every monitor shows the same data.

`exec` and `stream` callbacks keep the output context in which they were
started. A command started inside an `every_output` callback can therefore use
`output` safely in its completion callback.

### JSON

`nitroget.json_encode(value)` converts Lua values to JSON text. Invalid UTF-8
in strings is replaced with the Unicode replacement character before encoding.
`nitroget.json_decode(text)` converts JSON text back to Lua values. Lua arrays
with integer keys `1..n` become JSON arrays; string-keyed tables become objects.
Invalid JSON raises a Lua error, so use `pcall` when decoding external input.

### Streaming output

`nitroget.stream(command, stdin, on_line, on_done)` runs a command through
`sh -c`, writes `stdin` to it, and calls `on_line(line)` for each stdout line.
`on_done(ok, error)` runs when the process exits; `error` contains stderr.
Streams are stopped when the widget reloads, is parked, or calls
`nitroget.clear(id)`.

`on_line` and `on_done` run on the main thread, so they can update widget
globals directly. UI renders are coalesced during high-frequency output; every
line still runs its callback.

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

After a timer, click, slider change, text change, selection change, stream
update, or `exec` callback returns, Nitroget updates the affected views. The
script itself is **not** re-run; only the values that changed are written to
GTK.

### Running commands

```lua
-- asynchronous, with a result
nitroget.exec("playerctl metadata --format '{{title}}'", function(ok, text)
  if ok then track = text end
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

Saving a widget file or any `.lua` file in its folder reloads it after about
400 ms: the Lua script runs again once for the widget path, timers restart,
shared Lua state is reset, and every monitor copy rebuilds.

Changing the anchor/offset in `config.toml` or `nitroget placement` moves the
window without reloading the script, so widget state survives a move. Changing
monitor, layer or size builds a new surface and re-renders it; shared Lua state
is preserved.

When every copy is parked, timers and streams are drained. The script runs again
when a copy returns.

## Validation

```sh
nitroget check --widgets ~/.config/nitroget
```

This parses the template, loads and compiles the Lua (including `src` scripts),
applies the config and prints the effective placement of each widget. Errors and
ignored attributes are reported with the file name. Modules loaded with
`require` are resolved when the widget runs.

## Limitations

- The element vocabulary is fixed; there is no arbitrary HTML or canvas.
- `{#each}` is index-based. Reordering items with the same count updates rows by
  position rather than moving widgets.
- Every expression is re-evaluated on each update, though only changed
  properties are written to GTK.
- Widgets on the `bottom` layer get pointer input. Keyboard focus is requested
  on demand while a visible `entry` exists.
- Text input is single-line. Multi-line editors and general overlay/popover
  elements are not supported.
- `scroll` scrolls vertically only.
- Panels are edge-attached. Exclusive zones cannot reserve arbitrary rectangles
  in the middle of an output.
- All monitor copies of a widget share one Lua state. A runtime error or global
  mutation affects every copy.
- GNOME/Mutter does not implement layer-shell; it is not supported.
- Widgets run with your user privileges and can execute commands.
