# The Agents Office: implementation plan

> **Status, 2026-09-22.** Steps 1 to 7 shipped: the rename, the clock rule, the window,
> the setting, the button and `Ctrl+Shift+G`, seating, and the toolbar. Still to do, in
> order of how much they matter: persisting seat assignments across a restart, picking the
> zoom in physical pixels so a fractional display scale keeps the art on whole pixels, and
> verifying the two macOS calls (tiling opt-out, multi-screen positioning) on a Mac, since
> neither can be compiled from Windows.

A second popout window beside the Overview, showing every **running agent** as a desk in a
pixel-art office. The Overview is an inventory of terminals; this is a picture of agents.
Different scope, so a different window, and both can be open at once.

Off by default. Setting off, no button, no shortcut, no code path. Window closed, nothing
exists. Window open with nobody mid-turn, a still image costing zero frames.

---

## 0. What is already built

On this branch, `src/floor.rs` (1900 lines) and `src/bin/floor_demo.rs`, with tests:

- `render_at(&Scene, t, scale) -> Buf`, a pure function to a display-resolution RGBA buffer.
- `Scene { desks: Vec<Desk>, labels: Vec<(String, String)>, show_names, selected, weather }`,
  with `spawn_named`, `remove_last`, `occupied`, `count`, `animates`, `size`.
- `Desk { Empty, Idle, Running, Ready, Working, Attention, Done }`, deliberately the same
  vocabulary as `pane_dot`.
- `hit(x, y, slots) -> Option<usize>` and `slot_origin(i)`, so a click maps to a slot from the
  same constants the painter uses.
- Nameplates drawn into the buffer from a bundled bitmap face, `Scene::show_names` to hide.
- Four rules held by tests: a desk never moves, the reserved colours belong to their states,
  nothing is random, and only real activity animates.

Nothing is wired to a `Session`. Everything below is the wiring.

---

## 1. Rename `floor` to `office`

Mechanical, no behaviour change, its own commit so the real diffs after it stay readable.

| Now | Becomes |
|---|---|
| `src/floor.rs` | `src/office.rs` |
| `src/bin/floor_demo.rs`, bin `floor-demo` | `src/bin/office_demo.rs`, bin `office-demo` |
| `floor::render_at`, `floor::hit`, `floor::rows` | `office::…` |
| `FLOOR_WEATHER`, `FLOOR_T` | `OFFICE_WEATHER`, `OFFICE_T` |
| `%TEMP%\ops-floor\` | `%TEMP%\agents-office\` |
| "Ops Floor" in the CHANGELOG entry and docs | "Agents Office" |

`FLOOR_Y`, `FORE_H` and `FLOOR` stay: those are the floor of the room, which is still a
floor. Incidental win, `floor` currently collides with `f32::floor` throughout `iced_shell.rs`
for a reader; `office` does not.

## 2. Only a turn in flight earns a clock

`Desk::animates()` returns true for `Working | Attention`, and the andon pulses. But
`needs_fast_tick` (`iced_shell.rs:8272`) deliberately excludes `Attention`: when true idle
landed in 1.0.11 the attention dot was changed from pulsing to solid precisely so a session
parked at a prompt could not pin the UI at 60fps, and the same change killed the usage
countdown for the same reason.

An agent can sit on a permission prompt for hours. As written, one blocked agent holds a
clock open the whole time.

So the andon lights and stays lit; the colour was always what caught the eye. Then
`Desk::animates()` is `Working` only and a room of blocked-but-idle agents costs nothing.

The weather change in §7 is the other half of the same idea and lands with it:
`Scene::animates()` becomes exactly "is any desk `Working`", with nothing else in the room
able to ask for a clock.

**Lands first and stands alone**, independent of everything else here.

## 3. The window

Built from the Overview's template, which is complete and proven. Every reference below is
`src/bin/iced_shell.rs` unless said otherwise.

| Piece | Overview's version | Notes |
|---|---|---|
| `State` fields | `overview_window/_size/_pos/_focused/_maximized` (`:124-148`) | mirror as `office_*` |
| Window settings | `overview_settings` (`:1382`) | borderless on Windows, transparent titlebar on macOS |
| Open | `open_overview` (`:1406`) | note the post-open `move_to`; at-creation position is unreliable |
| Per-window content | `view` dispatch (`:2987`) | add a branch before the `main_view` fallback |
| Title / theme | `:9287`, `:9299` | `"Arbiter · Agents Office"` |
| Toggle | `Message::ToggleOverview` (`:2628`) | take-or-open, `save_session` either way |
| Close | `WindowClosed` (`:2639`) | clear the id |
| Geometry | `WindowMoved/Resized/Focused` (`:2655-2794`) | each branches on window id |
| Persistence | `persist.rs:366-373` | `office_window: Option<SavedWindow>`, `office_visible: bool` |
| Reopen at startup | `:9410-9423` | **chain** `gain_focus(main_id)`, do not batch, or it steals focus |

**Content is the art and nothing else.** No titlebar, no logo, no usage footer. The art is
the drag region: one `mouse_area` over the image sending `DragOffice`, with the desks and the
toolbar button as the only regions that take a click instead.

**Minimum size 2x**, 760 by `office::height(slots) * 2`. `STRIP_H` is sized against that zoom,
and below it a nameplate does not fit.

Windows gotchas the Overview already solved and this window inherits: a borderless window
needs `resize_overlay()` stacked in (`:6688`) and a manual min-size clamp in `WindowResized`
(`:2684`), because winit does not honour `min_inner_size` for them; and
`winround::round_our_windows()` runs once on the first frame (`:6979`), so a window opened
later relies on `undecorated_shadow` instead.

## 4. Setting, button, shortcut

Follow `show_wol_button` exactly.

1. `persist.rs`: `#[serde(default)] pub show_agents_office: bool`, plus the entry in
   `impl Default` (false) and a line in the back-compat test at `:560`.
2. `Message::ToggleAgentsOffice(bool)` and an update arm that assigns and calls
   `save_session`.
3. `settings_toggle` row in **Settings → Display**, in a new "Agents Office" section under
   the existing "Overview" one.
4. New MDI path in `mod mdi_path` (`:6842`); an office or desk-lamp glyph.
5. `titlebar_row`: push `action_icon_btn(...)` right of the Overview button, gated on the
   setting, **and `+ 34.0` in `actions_w` (`:5762`)** or the tab and usage width budget
   over-estimates its free space.
6. `handle_key` (`:8551`): `Ctrl+Shift+G`. `Ctrl+G` is BEL and nobody types it deliberately,
   which is the reasoning already written into the code for `Ctrl+Shift+M`. Add a row to
   `shortcuts_dialog_view`'s `ROWS` (`:4097`) and bump the array length from 17.

## 5. Seating

**Who gets a desk.** Every pane where `session.claude_running()` is true, across all
workspaces. Flat, not grouped: grouping wastes the cap, since a workspace with one agent
would hold a whole row of five.

Consequence, stated once: `Desk::Idle` and `Desk::Running` become unreachable, because both
describe panes where Claude is not running. The office uses `Empty`, `Ready`, `Working`,
`Attention` and the transient `Done`. The art keeps all seven for the demo and the tests.

**The seat key is `PaneData.history_id`,** not `Session::id()`. `Session::id()` is re-minted
when a session respawns (reconnect `:1138`, shell switch `:2935`), so keying on it would move
an agent to a different desk on reconnect and break the room's founding rule silently.
`history_id` is globally unique, survives respawn, and is persisted (`persist.rs:66`).

```rust
/// Slot -> the agent sitting at it, for the life of that pane. Never reordered.
office_seats: Vec<Option<String>>,   // history_id
```

Refresh pass, cheap enough to run on any wake that could have changed a dot:

1. Clear seats whose `history_id` is no longer a running agent.
2. Seat unseated agents in the lowest free slot, growing by `COLS` up to `MAX_ROWS = 4`.
3. Fill `Scene::desks[i]` from `pane_dot(...)` and `Scene::labels[i]` from the workspace name
   and `PaneData.name`.

Iterate as `poll_connection_signals` does (`:1239`):
`for (wi, ws) in state.workspaces.iter().enumerate() { for (pane, d) in ws.panes.iter() }`.

**A seat is released the moment Claude exits, not when the pane closes.** Quitting Claude in a
pane frees its desk immediately and the pane carries on as a shell, which falls out of keying
the room on `claude_running()`. The consequence to know: relaunching Claude in that pane takes
the lowest free seat, which may not be the old one if something claimed it meanwhile. That is
the right trade for wanting seats freed promptly.

**Past twenty desks: no pagination, and no preemption.** Pagination puts state into an ambient
display, so a glance would no longer be enough. Preemption (letting a blocked agent take an
idle one's desk) was considered and rejected: it only bites above twenty *concurrently
running* agents, which is past the point where anyone is reading faces, and nothing is lost
when it does, because the Overview still lists that agent and the notification card still
fires. It is pure seating policy, so it can be added later if the case ever turns up.
Unseated agents wait for a free desk.

**Click a desk** maps through `office::hit` to a slot, then to `(wi, pane)`, then reuses
`Message::JumpTo(wi, pane)` (`:2864`), the same handler the Overview's rows use.

## 6. The toolbar

A gear in the top-right corner over the empty ceiling right of the last clerestory window
(x 354 to 380 in room coordinates, which no desk ever occupies). Shown on hover, hidden
otherwise, switched instantly: a fade is a clock. The dropdown is modelled on `wol_menu_view`
(`:5343`); note its hardcoded right-hand offset, which is the kind of thing that rots.

- **Always on top** — default **off** (the Overview defaults on; this does not). Applied live
  with `window::change_level`, as at `:1935`.
- **Show names** — default on. Already plumbed: it is `Scene::show_names`, honoured by
  `render_at`, so this is a one-line binding.
- **Weather** — Clear (default), Sunny, Cloudy, Rain, Snow, Fog, as a manual pick. No
  auto-drift: `Weather::drifting(t)` changes with time, so leaving the sky on automatic means
  a clock running for a decoration nobody asked for, which is the one thing the no-polling
  rule is actually about. Rain and Snow are marked, being the only decoration that costs a
  clock at all (`Weather::moves`), and they put themselves away (§7).
- **Freeze motion** — default off. Pins the room to a still frame.
- **Close** — there is no titlebar close button.

Not carried over from the demo harness: spawn, remove, set-state, the scale readout. Test
instruments.

## 7. The clock

Its own subscription and its own message, batched in `subscription` (`:8310`) beside the
existing 16ms tick. Not `Message::Tick`, whose handler also does chrome-init and save-dirty
work.

```rust
office_window.is_some() && !office_frozen && scene.animates()
```

**12fps, not 60.** The fastest thing in the art is `SPRITE_FPS = 7` and the spinner steps 3
times a second, so 12fps resolves everything the art does and matches its deliberately slow
pacing.

Measured on a release build: **0.45 ms/frame at 5 desks, 1.35 ms at 20**, so 0.5% to 1.6% of
a core while agents are working, and nothing at all otherwise.

### The weather never asks for a clock

`Scene::animates()` drops `weather.moves()` and becomes "is any desk `Working`". Rain and snow
then animate purely as a side effect of a clock a turn in flight had already justified, and
never request one of their own. When the last turn ends the drops simply stop where they are.

The sky is **never changed** to achieve this. An earlier draft had precipitation expire to
Clear after a minute of no `Working` desk, and that was wrong twice over. It would have made
the sky encode agent state, when this module's whole position is that the weather "encodes
nothing about any agent"; and the minute was not free, since it extended the clock by 60
seconds of 12fps every time the agents went quiet, which over a day is a great many frames
bought for a decoration.

Frozen rain is the right picture anyway. An idle room is a still image, and in a still image
rain does not move either.

`Weather::moves()` stays, because it still says truthfully which skies have moving parts; it
just no longer forces a clock. `tests::a_quiet_room_needs_no_clock` changes to assert the
opposite of what it asserts today: that `animates()` ignores the weather entirely.

## 8. Build order

Each step compiles, passes tests, and is independently useful.

1. **The andon** (§2). Re-render the sheet, confirm the amber still reads.
2. **The rename** (§1).
3. **The window** (§3), with a hardcoded fake scene, so it can be judged before data.
4. **Setting, button, chord** (§4).
5. **Seating** (§5): the seat vector, the refresh pass, `pane_dot` mapping, click to `JumpTo`.
6. **The toolbar** (§6).
7. **The clock** (§7), and re-measure idle CPU.
8. **Persist** the window geometry and the seat assignments, so desks survive a restart.

## 9. Tests and verification

Existing `office::tests` keep passing unchanged, including `reserved_colours` and
`a_quiet_room_needs_no_clock`, which is what stops the wiring quietly breaking the art.

New, pure, under `cargo test --lib`:

- A seat is not reassigned while its agent lives, across a respawn that changes `Session::id()`.
- A freed seat is reused by the next agent, lowest slot first.
- The room grows a row at six agents and stops at `MAX_ROWS`.
- `Scene::animates()` is false for a room of `Attention` agents under a still sky (the §2
  regression).
- A pane with Claude not running gets no desk.

By hand, per CLAUDE.md:

- **Run the debug build**, not just `--release`: iced states layout contracts as
  `debug_assert!` and release compiles them out. Check `panic.log` in the data dir.
- **Idle CPU** with the window open and every agent quiet: expect the same ~0.16% as without
  it, and a brief 12fps only while something is mid-turn.
- Open and close, drag, resize, move to a second monitor, restart, confirm it comes back where
  it was and the main window still takes focus.
- Reconnect a pane and switch its shell: its desk must not move.

## 10. Decisions taken

- **Cap 20 desks**, four rows of five.
- **`Ctrl+Shift+G`**, since `Ctrl+G` is BEL and nobody types it deliberately.
- **Nameplates carry the workspace and pane names outright**, so nothing depends on decoding
  the shirt colour; the shirt is the glance, the plate is the answer.
- **Flat seating, no grouping by workspace.** Grouping wastes the cap.
- **Seats are released when Claude exits**, not when the pane closes.
- **No preemption and no pagination** (§5).
- **All six skies ship**, as a manual pick with Clear as the default and no auto-drift. The
  sky is never changed by agent activity; it just stops moving when no desk is `Working`
  (§7).
