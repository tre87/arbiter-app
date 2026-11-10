# The agent banner: notes from building it

> **Status, 2026-11-10.** Shipped on the `agent-banner` branch, off by default. It
> replaced the Agents Office (`docs/AGENTS-OFFICE-PLAN.md` describes that, and is
> history now). `CLAUDE.md` has the short version of everything here; this file is the
> long one: what was built, what was tried and thrown away, what was measured, and what
> is still open.

A strip of pixel art at the top of a terminal that runs an agent. A robot works
outdoors while the agent works, under a day, night and weather cycle; each sub-agent
the agent starts is a smaller robot of its own.

## Where things are

| File | What it holds |
|---|---|
| `src/agent_banner.rs` | The art. A pure renderer plus `Troupe`, the one stateful part. |
| `src/bin/pane_banner.rs` | The banner in real panes: when it shows, sizing, frames. |
| `src/bin/banner_preview.rs` | A test window with one fake pane (`ARBITER_OPEN_BANNER=1`). |
| `src/agent_state.rs` | What counts as working (`lifecycle`, `waiting_on_agents`). |
| `src/agent/claude.rs` | Claude's hooks mapped to events, sub-agents included. |

Settings: `show_agent_banner` (off by default, also the robot button in the titlebar)
and `banner_hide_idle` (on by default).

## The art

- **Drawn, not imported.** Every pixel is a rectangle fill into an RGBA buffer. No
  assets, no atlas, no dependency.
- **`render(scene, w, t)` is pure.** What varies comes from `hash`, never an RNG or
  stored state, so any frame can be reproduced and tested.
- **46 art pixels tall at any width** (`H`). Drawn at 2 logical pixels per art pixel,
  rounded to whole physical pixels (`pane_banner::zoom`), so 92 px on screen.
- **The world is anchored on the centre column.** A wider pane shows more land on both
  sides; a resize never moves scenery already in view. The centre is composed by hand
  (tree, pine, lake) so the narrowest strip is still a picture; further out each
  56-pixel cell holds what its hash says.
- **Light.** `light(day, overcast)` gives the sky gradient, an ambient tint and a night
  factor. Things lit by the day are drawn through `Paint::put` (tinted); things that
  glow (eyes, chest light, antenna lamp, sun, moon, stars, flame) through
  `Paint::glow` and stay bright at night.
- **Time.** A day is `DAY_SECS` (240) scene seconds; weather changes every
  `WEATHER_SECS` (40), weighted towards fair. Scene time only advances while the agent
  works, and is never reset for the life of the terminal, so the next turn carries on
  with the same sun and weather. It is not saved across an app restart.

### Robots

- Main robot 11 by 19 art pixels, helpers 9 by 14 (`MAIN`, `HELPER`). The main one has
  a blue chest light; each helper slot has its own body colour.
- **The antenna lamp is the state:** blue working (blinking), amber needs you, green
  done, grey idle.
- **Eyes** stay inside the visor with a column of black around them; the back eye sits
  a column nearer the middle than the front one, which is what makes it look where it
  is going.
- **No body bob.** It was tried and disliked. The antenna tip sways a pixel each way
  instead; arms, tools, legs, blink and lamp do the rest. Alternatives noted and not
  built: a head nod, a chest light pulse, a lean on each tool stroke, eye glances.
- **Rounds** (`stop`, `act`): three jobs near one another, then somewhere else in the
  strip. A stop within `WALK_MAX` (34) is walked to, a further one flown to by jetpack
  in an arc. Eight jobs, never the same twice running: watering a plant that grows and
  flowers, a fence built post by post, a mound that ends with a sapling, crates
  stacked, a landscape painted on an easel, a log split into a woodpile, apples picked
  from a bush into a basket, a campfire laid and lit (it glows at night). While it
  snows, one stop in three is a snowman instead (`Stop::job_in`). The last finished
  piece of work stays standing until the next is done.
- **Rare things**, each from a hash so they need no state (`LUCKY_ONE_IN`,
  `BIRD_ONE_IN`, `STAR_ONE_IN`):
  - a dig that turns up a treasure chest instead of a sapling, about one dig in five
    (a chest per robot every seven minutes of work or so);
  - a bird that perches on a robot's antenna for one stop, about one stop in 32;
  - a shooting star on a clear night, about once in eleven minutes of work, with
    every robot on the ground looking up for three seconds (`Paint::gaze`).
- **States other than working are still pictures:** idle fishes in the lake (main
  robot, no helpers), needs-you raises an arm under an amber "!" bubble, done raises
  both arms beside a green flag.

### `Troupe`: the one stateful part

It remembers where each robot was drawn, so that:

- a robot whose place has jumped flies there instead of appearing there (the strip
  was resized, the turn ended and it goes fishing);
- a new helper flies in from outside the frame;
- a helper that is no longer wanted flies off by the nearer edge, climbing.

A robot left far outside by a narrowed strip starts from just past the edge, so it is
in view on the next frame. `settling()` says a robot is still in flight, which is the
only reason a banner asks for frames while its agent is not working.

### Helpers are slots

`Scene::helpers` is a bit mask: bit `i` is a helper in slot `i`. A slot is a robot of
its own (colour, round). `pane_banner::PaneBanner::seat` keeps each sub-agent, by the
agent's own id, in one slot for as long as it runs, so the robot that leaves is the
one whose sub-agent finished. It used to be a count, and the newest robot left
whichever sub-agent was done.

How many are drawn is a matter of width: one per 30 art pixels past the main robot's
own (`helper_room`), the lowest slots first. `MAX_HELPERS` (12) is only the ceiling,
reached at 380 art pixels (760 px). A sub-agent beyond the slots waits without a
robot until one frees.

## In a pane

- **Frames are drawn in `update`** (`pane_banner::refresh`, after every message), since
  `view` cannot cache. Only frames that changed, and only for the workspace on screen;
  other workspaces just keep time.
- **No clock of its own.** Frames come from the app's fast tick, which runs while any
  agent works, plus `needs_fast_tick` while a robot is in flight. The art steps at 20
  fps (`FRAME_MS`).
- **Too small:** under `MIN_W` (80) art pixels wide or `MIN_PANE_H` (280 px) high the
  pane gets no banner and an icon in its header that says why (`pane_banner::fit`).

### Overlay or rows

- **`banner_hide_idle` on (default): an overlay.** The banner is a layer over the
  terminal's first rows, in the terminal's own stack. The PTY is never resized for it;
  what it covers is hidden while it shows.
- **Off: rows of its own.** The banner stays between turns, so it sits in the pane's
  column above the terminal, which is that much lower. The PTY is resized when the
  agent starts and exits.

The slot in the column is always there, at no height without a banner of that kind,
so the terminal keeps its place in the widget tree (iced resets a widget's state when
its slot changes type).

### When it shows

Hidden when idle, the banner is there exactly when the pane's working bar is: one
rule for both (`pane_banner::wanted`, and `overlay` checks `working` again at view
time). **Do not add holds to the banner.** See the next section.

## What was tried and thrown away

In order, all on 2026-11-10. Each looked reasonable and each was wrong.

1. **Taking rows from the terminal on every show and hide.** Each one resized the PTY
   and made the agent repaint its whole screen: the content jumped. It also looped:
   the banner went, the repaint read as work, the banner came back, for as long as
   the pane sat idle. The misread was an old "Waiting for N background agents" row
   in the transcript, which counts as live while a repaint has not drawn the input
   box yet. Fixed twice over (`set_waiting_background` ignores a row first seen in a
   repaint window; `PaneBanner::follow` will not flip back within 4 s unless the
   opposite has held for 3 s), then made moot for the default mode by the overlay.
   Both guards still apply to the rows mode.
2. **Holding the banner while sub-agents ran, in the banner only.** Better, but the
   bar and the dot still dropped, so the two disagreed.
3. **A 30 second "done" linger after every turn.** Bridged the gaps, and read as
   "work did not stop" after a cancel, and as lingering after Claude was done.
4. **Esc ending the linger.** Helped the cancel case only.
5. **Waiting 0.8 s before leaving.** Meant to stop a blink at a turn's end; it still
   blinked, and added a frozen picture.

What worked was none of these: fixing what counts as working for the whole pane.

## What counts as working (the real fix)

A Claude given long work does not do it in one turn. It starts sub-agents in the
background, ends its turn at once (the Stop hook fires), and is woken as each
reports, for a turn of a second or two. One test job was about 45 turns with gaps of
8 to 20 seconds. Arbiter judged work per turn, so the bar, the dot, the banner and
the "finished" card followed every one.

Now `AgentHandle::waiting_on_agents` holds the lifecycle at Working while any
sub-agent Claude reports is running, and neither a Stop nor a row end counts as a
finish meanwhile. The signals are all Claude's own.

### Measured from Claude 2.1.322

A live interactive Claude in a headless pane, told to start two sub-agents and a
`sleep 25` in the background:

- **`SubagentStart` / `SubagentStop`** carry `agent_id`; start and stop match.
- **Every `Stop` carries `background_tasks`**, for example:
  `[{"id":"a937…","type":"subagent","status":"running","description":"…","agent_type":"general-purpose"},
  {"id":"bnzm…","type":"shell","status":"running","description":"…","command":"sleep 25"}]`.
  The docs say it exists to tell "session is done" from "session is paused waiting
  for background work". The final Stop had an empty list.
- **`SubagentStop` also carries the list**, but it is stale there (it still names the
  sub-agent that is stopping), so only its `agent_id` is used.
- **Claude fires `SubagentStop` for helpers of its own it never announced:** ids in no
  list, `agent_type` empty, about a second after each Stop. This is why the running
  sub-agents are a set of ids and not a counter: an unknown id changes nothing.
- **`UserPromptSubmit` fires when Claude is woken** by a report (the prompt is an
  `<agent-message>` or a `<task-notification>`), as well as for a typed prompt. It
  follows the Stop before it within a tenth of a second. Used for the wake-ups only
  (see "An empty list is not always the end").
- **`idle_prompt`** arrived 60 seconds after the last Stop. The docs say Claude holds
  it back while a background agent runs.

### How the set is kept

- Starts and stops keep it current between turns.
- `AgentEvent::BackgroundAgents`, built from a Stop's running `subagent` entries and
  emitted before `TurnEnded`, replaces it: a lost report is put right at the next
  turn end.
- `idle_prompt` empties it.
- Esc or Ctrl+C, a new conversation and the agent exiting empty it.
- When the last one stops, `agents_done_ms` holds Working for one TTL (2 s) until the
  turn Claude is woken for draws its row; a Stop since ends the hold.

Result in the live test: working without a break from the prompt to the last reply,
one finish instead of three.

### An empty list is not always the end

The first real run after that change (its debug log, 2026-11-10) showed the pane
idle for a second, twice in one job, each with a false "finished":

```
20.2  Stop            tasks = [subagent running]      held, as intended
20.5  SubagentStop    the one that was running
21.5  Stop            tasks = []                      read as the end: idle, finish
22.7  working again   Claude woken for that last report
```

The sub-agent finished while Claude was in a wake-up turn of a second. That turn's
Stop listed nothing, which was true, but the report had not been heard: Claude is
woken for it as soon as the turn is over. So:

- `UserPromptSubmit` is registered, only to hear the wake-ups. Its prompt then
  starts with `<agent-message` or `<task-notification`; a typed prompt emits nothing,
  because the hook may also fire for input that starts no turn.
- A sub-agent that a Stop has named (it outlived a turn, so it is a background one)
  sets `report_owed_ms` when it stops. Later than the last wake-up, the report is
  still owed.
- A Stop with a report owed holds Working for `WAKE_WAIT_MS` (1.5 s) and judges the
  finish only then: a wake-up in between cancels it, none means the turn was the end.
- A turn start reported after a stop now wins over the "just stopped" rule in
  `lifecycle`, or the wake-up itself would read idle for a moment.

Known cost: Claude can hand a report over in the middle of a turn that is still
calling tools. Then nothing is owed, Arbiter cannot tell, and the pane stays working
for 1.5 s after the real end. A sub-agent that starts and finishes inside one turn
is taken for a foreground one and owes nothing; if it was a background one, the
second of idle comes back for that case.

## Decisions worth knowing before changing things

- **A shell is not work.** `background_tasks` lists shells too, and they are left
  out. Nothing tells `sleep 20` from a dev server, and a server would hold the pane
  working all day (fast tick on, "finished" never raised). So a Claude waiting on a
  background command reads as idle between turns. See "A background shell is not
  work" in `CLAUDE.md`.
- **The one hour cap stays** (`SUBAGENT_MAX_MS`). It was going to be shortened. It
  should not be: the set is only corrected at a turn end, and a Claude paused on one
  long sub-agent has no turn end until it reports, so a shorter cap would show a
  real 20 minute sub-agent as idle. A lost stop report is already put right by the
  wake-up turn's Stop or by `idle_prompt`, so the cap only matters for a sub-agent
  that dies without Claude being told, which is rare.
- **Continuing a conversation used to flash working.** Not the banner's doing, but
  found through it: a continued Claude paints its transcript in chunks whose old
  spinner rows differ, which paired like a spinner animating. `START_QUIET_MS` (3 s
  after an agent is bound) stops spinner frames starting work; Enter ends the window.
- **Esc is every Esc.** Arbiter cannot tell a cancel from dismissing a menu, so any
  Esc empties the sub-agent set. If the sub-agents are in fact still running, the
  next Stop names them and the pane turns working again.

## Open, and deliberately not done

- **Other kinds of background work.** `background_tasks` has types never seen here:
  `monitor`, `workflow`, `teammate`, cloud session, MCP task (from the docs; entries
  also carry `server` and `tool` for monitor and MCP, `name` for workflow). None is
  counted. An agent-team job (teammates) would probably flicker the way sub-agents
  did. Capture a real Stop payload with one (`ARBITER_CLAUDE_DEBUG=1` logs every hook
  payload raw) before adding it; `session_crons` (scheduled wake-ups) is the same.
- **Frame rate while paused on sub-agents.** The pane is working, so the 60 fps tick
  runs for the whole wait, as for any working pane. Not measured. Lowering it means a
  second cadence, and the working bar is drawn from the clock each frame, so it would
  sweep in steps. Measure idle CPU and GPU during a long wait before deciding.
- **Copilot and Codex report no sub-agents.**
- **A helper that no longer fits after narrowing** flies off like one that is done;
  it comes back if the strip widens.
- **Not saved:** scene time and helper slots start over with the app.

## Ideas kept for later

Suggested on 2026-11-10 and not built. Adding a job means: a `Job` variant, an entry in
`JOBS` (or a condition in `Stop::job_in`, as for the snowman), what it leaves on the
ground in `made`, and the robot's arms in `robot`. A rare thing is a hash test on the
stop or on scene time. Add a frame for it to the `sheet` test and look at it.

**Jobs**

- Planting a row: seeds go in one by one and become a row of sprouts.
- Sweeping or raking: a pile of leaves that grows.
- Repairing another robot: a wrench on a helper's back panel, with sparks. Only with
  helpers present, and it needs the two robots at the same stop.
- Carrying water: a walk to the lake with a bucket and back.
- Stargazing through a telescope, at night only.
- Flying a kite, on clear days only.

**Rare things**

- A rainbow after rain clears. Needs the weather before this one, which a pure
  `render` can get from `Weather::drifting` at an earlier time, but not when the
  caller fixes the weather.
- A UFO that drifts across the sky and blinks at the robots.
- A robot dance: every robot stops and does a few moves together.
- A cat that wanders through, and a robot that pets it.
- A tiny rocket: a long build that ends with a launch and a smoke trail.

**Idle only** (seen only with "Hide the banner when the agent is idle" off)

- A fish on the line. As animation (a bite, a lift) it would break the rule that an
  idle pane draws no frames. It can only be a still: now and then the idle robot is
  drawn with a fish on its line, chosen by a hash of the scene time the turn ended
  at, with no motion.
- A nap under a tree at night with a "z" over it. The same applies: a still.

## Working on it

- **Look at frames.** `cargo test --lib agent_banner::tests::sheet -- --ignored
  --nocapture` writes PNGs to `<temp>/agent-banner`. Every correction to the art came
  from looking at a frame, not from reading the code that drew it.
- **Drive it by hand:** `ARBITER_DATA_DIR=/tmp/arbiter-test ARBITER_OPEN_BANNER=1
  cargo run --bin arbiter` opens the test window: state, helper count, hour, weather,
  fast forward (a day in ten seconds), freeze.
- **See why a pane is working:** `ARBITER_CLAUDE_DEBUG=1` logs each lifecycle change
  with every input it read (`lifecycle:` lines in `<temp>/arbiter-claude-debug.log`),
  and every hook payload raw.
- **Live checks** (a signed-in Claude; each spends a few short turns):
  - `cargo test --lib agent_shim::tests::live_claude_reports_a_subagent -- --ignored
    --nocapture`
  - `ARBITER_LIVE_CWD=<a folder Claude trusts> cargo test --lib
    session::tests::live_background -- --ignored --nocapture` fails if the pane reads
    idle mid-job or counts more than one finish.
- **A prompt that exercises it:** ask Claude to start a few general-purpose
  sub-agents at once, then two one after the other, then more than fit, replying
  briefly as each finishes. Foreground `sleep` is blocked for Claude, so give the
  sub-agents something slow to produce instead (a long list of numbers).
