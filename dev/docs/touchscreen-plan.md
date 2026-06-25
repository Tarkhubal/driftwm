# Touchscreen plan

Tracks touchscreen work beyond the initial input PR (#163, "feat: touchscreen
support and multi-touch canvas gestures"). That PR lands raw touch input +
canvas gestures + the config surface; everything below is deliberately deferred
to maintainer-owned follow-ups so the contributor PR stays scoped to touch
*input*.

## What's already in place

- Touch seat capability (`seat.add_touch()`), slot-based multi-touch tracking
  (`input/touch.rs`), Wayland client forwarding (screen → surface coords).
- Canvas gestures: 1-finger drag pans (empty canvas), 2-finger pinch zooms,
  3-finger swipe navigates.
- **OSK protocol stack is wired already** — `input-method-v2`
  (`InputMethodHandler`, `handlers/mod.rs`), `text-input-v3`, and
  `virtual-keyboard-v1` are all delegated, plus wlr-layer-shell. An external OSK
  (squeekboard via input-method, wvkbd via virtual-keyboard) can connect, render
  as a bottom layer surface, type into apps, and auto show/hide via smithay's
  text-input↔input-method bridge. The OSK stays an external program (same
  philosophy as launcher / lock / screenshot).

## Config surface (final shape — done in #163)

Principle: `[input.*]` is device config; compositor behavior lives in its own
sections. driftwm already follows this (`[input.mouse]` device vs `[mouse]`
bindings); niri does the same (`input.touch` = off/calibration/map-to-output,
behavior in a top-level `gestures` block).

- `[input.touch] enable` — keep (device on/off; future home for
  `calibration_matrix`, `map_to_output`).
- `[navigation] touch_speed` — pan multiplier, sibling of `trackpad_speed` /
  `mouse_speed`.
- `[zoom] touch_speed` — pinch multiplier. Joins the future `trackpad_speed`
  (pinch) / `mouse_speed` (wheel) zoom multipliers from the separate zoom-speed
  issue.
- top-level `touch_to_focus` — focus-model behavior, sibling of
  `focus_follows_mouse`.
- `enable_canvas_gestures` and `swipe_threshold` — dropped / hardcoded. Both are
  gesture-model knobs that the bindable rework below subsumes; shipping them now
  just means deprecating them later. `enable` (whole-device off) is the only
  touch toggle until the bindable model lands.

## Follow-up: bindable `[touch]` model + interaction rework

Make touch gestures bindable, parallel to `[gestures]` / `[mouse]`, with a
separate `[touch]` behavior section (NOT folded into `[gestures]` — that's
trackpad/libinput relative-gesture semantics; touch is absolute-positioned with
a real on-window/on-canvas distinction baked into where the finger lands).

Decide the surface before it ships so it's release-stable even if the
implementation lands incrementally.

Target interaction model (escalation: fingers go content → system):

- **1 finger** — window = content (forward), empty canvas = pan.
- **2 finger** — window = app's own pinch/scroll (forward), empty canvas = zoom
  viewport.
- **3 finger** — compositor pan + pinch, **anywhere incl. over a window** (apps
  don't claim 3-finger touches, so it's unambiguous). Fixes the current
  limitation where any finger on a window kills gestures, so you can't
  pan/zoom over a dense canvas.
- **4 finger** — global navigation: swipe = navigate-nearest, pinch-in/out =
  overview / home-toggle. Position-independent.
- **3-finger tap** — center-window (position-aware: centers the tapped window;
  empty-canvas fallback = center focused window). Replaces the touchpad's
  `4-finger-hold` — hold has no release-into-action idiom on glass and occludes
  the screen. 3-finger tap is free on touch (it's only middle-click on the
  touchpad's click-emulation layer, a different layer entirely) and lands
  cleanly, unlike an error-prone 4-finger simultaneous tap.
- **3-finger double-tap** (no drag) — fit-window (maximize toggle). Mirrors
  desktop double-click-titlebar-to-maximize.
- **3-finger double-tap + drag** — move-window. Mirrors the existing trackpad
  `3-finger-doubletap-swipe = move-window` exactly.

### Window resize on touch — deliberately minimal

The canvas/zoom model demotes resize: window size and apparent size are
decoupled, so "make this bigger" is served by pinch-zooming the *viewport*, not
resizing the window. Precision resize (true content dimensions) is a power-user
task and touch is the worst modality for it — leave it on keyboard/mouse
(`resize-window`).

- The discrete "fill the screen" intent is covered by `fit-window` (3-finger
  double-tap above), not a drag.
- Don't make the 8px resize border touch-draggable (far below a ~40px fingertip;
  widening it conflicts with content drags near window edges) and don't use
  2-finger-on-window (that's the app's own pinch).
- Precision `resize-window` stays optional, exposed only via the bindable model
  as a 3-finger on-window gesture variant if ever wanted. The action already
  exists; it just needs a trigger.

Rule of thumb: 1–2 fingers over a window belong to the app; reserve 3+ for the
compositor. This is the touchscreen analog of "scroll/pinch → apps, 3–4 finger
swipes → compositor" on the touchpad, so touch ends up consistent with the
trackpad model rather than a special case.

Notes:
- Touch's absolute position makes on-window/on-canvas/anywhere contexts cleaner
  than the trackpad (no cursor-derived ambiguity).
- The gesture-internals bugs in the #163 state machine (swipe-origin divisor,
  stale pinch baseline on finger-count change) are subsumed by this rework — fix
  in #163 only what's needed for it to merge correctly; the rest gets rewritten
  here.

## Follow-up: cursor hide-on-touch

Hide the pointer when touch starts, restore on next pointer (mouse/trackpad)
motion — standard mutter behavior. #163 does none of this, so a stale arrow sits
mid-screen during touch use.

Shape:
- A separate `hidden_by_touch` bool on `CursorState` — do NOT overwrite
  `cursor_status` with `Hidden` (that field is client-owned; clobbering loses the
  app's requested shape on restore).
- Set on `TouchDown`, clear on the next pointer-motion handler in
  `input/pointer.rs`.
- OR it into the hidden gate in `render/cursor.rs::build_cursor_elements`
  (alongside `CursorImageStatus::Hidden => vec![]`). That one gate also clears
  the KMS hardware-cursor plane on udev (plane is driven from the same render
  elements), so no separate udev path.
- Touch routes through the *touch* handle, not the pointer, so
  `pointer.current_location()` never moves during touch — the cursor reappears
  where it was, no extra bookkeeping.

## Follow-up: OSK camera-positioning (biggest lever for tablet usability)

Protocols are wired (above); the gap is positioning. When a bottom-anchored OSK
appears it occludes the lower screen and the focused text field disappears
behind it. The infinite-canvas-native fix:

- Read the text-input **cursor rectangle** (`set_cursor_rectangle`,
  surface-local), transform to screen space via window canvas-pos → camera/zoom,
  and if it lands in the OSK-occluded band, **animate the camera up** so the caret
  clears — reusing the existing focus-to-window animation. This beats the mobile
  "shrink the fullscreen app" and desktop "exclusive-zone reserve" models: an
  exclusive zone only shrinks where new/maximized windows go, not where a
  floating focused window currently sits.
- `parent_geometry` (`handlers/mod.rs`) returns raw `window.geometry()` (window-
  local size, not camera-transformed) — this positions IME candidate popups
  (CJK completion, emoji). Verify they land correctly at non-1.0 zoom /
  off-origin camera.
- Cursor rect arrives in surface-local px; at zoom ≠ 1.0 the on-screen caret is
  scaled, so both the popup positioning and the camera-pan must multiply by zoom.
