# Nitroget

Desktop widgets for Wayland. A widget is one `.nitroget` file: a template, a
stylesheet and a Lua script.

## Install

Requires GTK 4.12+, `gtk4-layer-shell`, and a compositor with layer-shell
support (sway, niri, Hyprland, KDE, COSMIC, etc.).

```sh
cargo install --path .
```

## Usage

```sh
nitroget                  # run every widget in ~/.config/nitroget
nitroget placement        # move widgets with the mouse (Enter saves, Esc cancels)
nitroget quit             # stop the running instance
nitroget new my_widget    # create a starter widget
nitroget check            # validate templates and Lua

nitroget -w DIR           # use another widget folder
```

Only one instance runs at a time. Save a widget file and it
reloads automatically. Placement is stored in `config.toml` inside the widget
folder (`~/.config/nitroget/config.toml` by default).

## Example

`~/.config/nitroget/clock.nitroget`:

```html
<widget name="clock" width="320" height="150">
  <box class="card">
    <label class="time">{time}</label>
    <label class="date">{date}</label>
  </box>
</widget>

<style>
  .card {
    background-color: rgba(18, 18, 24, 0.72);
    border-radius: 18px;
    padding: 18px 22px;
  }
  .time { font-size: 44px; font-weight: 800; color: #f4f4f5; }
  .date { font-size: 14px; color: #a1a1aa; }
</style>

<script>
  local function tick()
    time = os.date("%H:%M")
    date = os.date("%A, %d %B")
  end

  tick()
  nitroget.every(1000, tick)
</script>
```

Widgets live in `~/.config/nitroget/`. More examples: [`examples/`](examples/).

**Do not install widgets without checking their code, as they can execute arbitrary commands with your user privileges.**

Full reference: [`docs/widgets.md`](docs/widgets.md).
