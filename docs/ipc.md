# IPC

driftwm exposes a small IPC over a Unix domain socket so external tools and
scripts can query and control the running compositor. The `driftwm msg`
subcommand is the built-in client; the wire protocol is plain line-delimited
JSON, so any language can speak it directly.

## `driftwm msg`

Run `driftwm msg <command>` from inside a driftwm session. Each command reads
when given no arguments and writes when given arguments.

| Command            | Example                  | Description                                              |
| ------------------ | ------------------------ | ------------------------------------------------------- |
| `camera`           | `driftwm msg camera`     | Print the camera position (viewport center)             |
| `camera <x> <y>`   | `driftwm msg camera 500 300` | Center the viewport on `(x, y)` (animated)          |
| `zoom`             | `driftwm msg zoom`       | Print the zoom level                                    |
| `zoom <level>`     | `driftwm msg zoom 0.5`   | Set zoom (animated); clamped to the supported range (out to fit-all, in to native) |
| `focus`            | `driftwm msg focus`      | Print the focused window's `app_id`                     |
| `focus <app_id>`   | `driftwm msg focus alacritty` | Focus a window by `app_id` substring (case-insensitive); navigates to it only if it's off-screen |
| `move`             | `driftwm msg move`       | Print the focused window's position                     |
| `move <x> <y>`     | `driftwm msg move 100 200` | Move the focused window                               |
| `close`            | `driftwm msg close`      | Close the focused window                                |
| `layout`           | `driftwm msg layout`     | Print the active keyboard layout                        |
| `state`            | `driftwm msg state`      | Dump camera, zoom, and the window inventory             |
| `quit`             | `driftwm msg quit`       | Shut the compositor down gracefully                     |

Add `--json` to print the raw JSON reply instead of the human-readable form:

```bash
driftwm msg --json state
```

A command that fails (bad value, no focused window, no match) prints an error to
stderr and exits non-zero, so scripts can branch on it.

### Coordinates

Window and camera positions use the same convention as
[window rules](window-rules.md) and the [state file](#state-file): positions are
a **center** point, with **Y pointing up**. So `move 0 0` centers the focused
window on the canvas origin, `camera 0 0` centers the *viewport* on the origin,
and positive `y` is above it.

## Wire protocol

The socket path is `$XDG_RUNTIME_DIR/driftwm/ipc-<WAYLAND_DISPLAY>.sock`
(permissions `0600`). The name is derived from the compositor's `WAYLAND_DISPLAY`,
so each instance owns a distinct socket and a client launched inside a session
automatically targets that session. Set `DRIFTWM_SOCKET` to point a client at an
explicit path.

The protocol is one JSON **request** per line, answered by one JSON **reply**
per line. A single connection may carry several requests; the connection stays
open until the client closes it.

A reply is `{"Ok": <response>}` on success or `{"Err": "message"}` on failure.

### Requests

| Request            | JSON to send                               |
| ------------------ | ------------------------------------------ |
| get / set camera   | `{"Camera":null}` / `{"Camera":[500,300]}` |
| get / set zoom     | `{"Zoom":null}` / `{"Zoom":0.5}`           |
| get / set focus    | `{"Focus":null}` / `{"Focus":"alacritty"}` |
| get / set move     | `{"Move":null}` / `{"Move":[100,200]}`     |
| close              | `"Close"`                                  |
| layout             | `"Layout"`                                 |
| state              | `"State"`                                  |
| quit               | `"Quit"`                                   |

### Responses

```json
{"Ok":{"Camera":{"x":500.0,"y":300.0}}}
{"Ok":{"Zoom":0.5}}
{"Ok":{"Layout":"English (US)"}}
{"Ok":{"Focused":"alacritty"}}      // or {"Ok":{"Focused":null}}
{"Ok":{"Position":{"x":100,"y":200}}}
{"Ok":"Ok"}                          // close / quit / camera-set etc.
{"Ok":{"State":{"camera":[-960.0,-600.0],"zoom":1.0,"windows":[
  {"app_id":"foot","title":"~","position":[0,0],"size":[800,480],
   "is_focused":true,"is_widget":false}
]}}}
{"Err":"no focused window to close"}
```

The `windows` array is the same shape driftwm writes to its [state file](#state-file),
focused window first.

### Talking to the socket directly

```bash
SOCK="$XDG_RUNTIME_DIR/driftwm/ipc-$WAYLAND_DISPLAY.sock"

echo '"State"'            | socat -t1 - UNIX-CONNECT:"$SOCK"
echo '{"Camera":[500,300]}' | socat -t1 - UNIX-CONNECT:"$SOCK"
```

## State file

For read-only polling (status bars, scripts), driftwm also writes a throttled
(~10 Hz) snapshot to `$XDG_RUNTIME_DIR/driftwm/state` — `key=value` lines plus a
`windows=` JSON array using the same window shape as `state`. Reading that file
avoids a socket round-trip when you only need to observe.

## Limitations

- `layout` is read-only for now (the write side needs an XKB-switch action).
- There is no event/subscription stream yet — poll `state` or the state file.
