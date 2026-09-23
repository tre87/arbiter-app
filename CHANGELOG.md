# Changelog

All notable changes to Arbiter are documented here. The format roughly follows
[Keep a Changelog](https://keepachangelog.com/); version numbers track
`Cargo.toml`. This changelog covers the **native** app (1.0.0 onward); earlier
history belongs to the prior Tauri/Vue web app it replaced.

## [Unreleased]

### Added
- **A desk turns green when its agent finishes.** For 30 seconds after a turn ends, the
  agent's desk in the Agents Office shows the green "done" state, then goes back to
  ready. It is set by the same turn end that raises "Claude finished", so it never turns
  green from a pause, and it clears at once if the agent starts working again or asks for
  input.

### Changed
- **The Agents Office is as small as its agents allow, and sizes itself.** The room has
  exactly as many rows as its agents need: a sixth agent opens a second row, and when a
  row's agents have gone, the desks below move up to close the gap, so five agents are
  always one row. The window follows the room both ways, growing for a new row and
  shrinking when one goes, and can no longer be resized by hand on macOS or Windows. It
  fits the room exactly at any display scale.

### Fixed
- **Text no longer flickers on Intel graphics (Windows).** Every textured thing in the
  window (terminal text, tab labels, icons) flickered on Intel integrated graphics, at
  launch, after a menu opened and while the pointer moved. That was Intel's Vulkan
  driver, so Arbiter now draws with DirectX 12 on Windows, falling back to Vulkan only
  where no DirectX 12 hardware is found. DirectX 12 used to draw every window that was
  not maximised slightly soft, which is why Vulkan was used before; that is fixed too, so
  text is as sharp on DirectX 12 as it was on Vulkan. Settings, Display, Graphics shows
  which one is in use, and can choose DirectX 12 or Vulkan instead of Automatic (applies
  after a restart; a choice with no graphics adapter falls back to the other). Setting
  `WGPU_BACKEND` still overrides both.
- **Programs started in an Arbiter terminal no longer inherit its graphics backend.**
  Arbiter passed the backend it picked for itself (`WGPU_BACKEND`) on to every terminal,
  so a program there that draws with wgpu, a second Arbiter included, used Arbiter's
  choice as if it were yours.
- **A desk held for an agent that left is released on time.** An agent that stops keeps
  its desk for 20 seconds in case its pane is only reconnecting, but the release waited
  for the next thing to happen in the app, so an emptied row could stay until you
  switched workspace or touched something. It now happens when the 20 seconds are up.

## [1.6.0] - 2026-09-23

### Added
- **Ctrl+Shift+H brings every window back to the main display.** The main window is
  centred there, the Overview goes to its top left corner and the Agents Office to its
  top right, each sized to fit. It works from any of Arbiter's windows, so the one the
  keyboard reaches can be the one that was lost; reach it through the taskbar or the
  Dock first. Listed in the keyboard shortcuts.
- **Aborting background agents ends the working state.** A pane is held as working while
  "Waiting for N background agents to finish" shows, but stopping the agents leaves that
  line in the transcript, with "All background agents stopped" under it, and the pane
  stayed working for good. The line now only counts as the last thing above Claude's
  input box. The same goes for a finished turn's "1 shell still running".
- **macOS: a Window menu, and the Dock lists your windows.** Right-clicking Arbiter in the
  Dock showed no windows, because winit's menu bar has no Window menu and AppKit keeps an
  app's window list through that menu. There is one now, with Minimize (Cmd+M), Zoom and
  Bring All to Front, and the main window, the Overview and the Agents Office all appear in
  it and in the Dock menu. The Agents Office is borderless, which macOS never lists on its
  own, so each window is added explicitly; notification cards are kept out. The main
  window is titled "Arbiter" there, and in the Windows taskbar and Alt+Tab, rather than
  "Arbiter native", a leftover from the move off the web app.
- **A file explorer beside the terminals.** Settings, General, "Show the file explorer" (off
  by default) puts a folder button in the titlebar and on Ctrl+Shift+F. The first press in a
  workspace asks for a folder, the way Open Folder does in VS Code; after that the button
  shows and hides the pane. It hugs the left edge, stays out of Ctrl+Shift+E, and is dragged
  to any width. The tree brings back the file-type icons, chevrons, git colours and
  right-click menu of the explorer retired with project workspaces, with a folder now
  carrying the strongest git state beneath it, and with New file, New folder, Copy path and
  Copy relative path added. A file watcher keeps it current without polling, and each
  workspace remembers its folder, width and expanded folders across a restart.
- **A built-in editor.** Double-click a file in the explorer, or right-click and Open, and it
  opens in an editor with line numbers, syntax colouring by file type, undo and redo, cut,
  copy, paste and select all, a tab per file and a right-click menu. It opens at once
  whatever the file's size, and the wheel scrolls it. A `.vue` file colours its template,
  script and style blocks with the HTML, JavaScript and CSS grammars at once. The editor
  takes the terminals' place
  while it shows and the explorer's toggle brings them back, so nothing typed into it can
  reach a terminal you cannot see. Open tabs are remembered per workspace, though a relaunch
  always opens on the terminals. A file changed by something else reloads by itself when you
  have no unsaved edits, and asks first when you do, checked when you switch to its tab and
  when the watcher reports it. Ctrl+S saves, Ctrl+W closes a tab, and quitting with unsaved
  work always asks, whatever the quit setting says.
- **The explorer no longer keeps a repo busy at idle.** Its per-file `git status` was the
  one status read without `--no-optional-locks`, so it refreshed `.git/index`, which the
  explorer's own watcher saw as a change, which ran it again: a loop once per debounce for
  as long as the pane was open. It cost 4.7% of a core on an idle repo; it is now 0.16%,
  the same as with the explorer closed.
- **The editor opens a file at once, and holds only what it is showing.** It used to lay the
  whole document out so the line numbers beside it stayed in step, which meant opening a file
  shaped every one of its lines three times over and ran the syntax parser across all of them,
  in the frame the file was asked for. The editor is the viewport again and scrolls itself,
  and the numbers are read from its own scroll each frame instead of being kept in step by
  brute force, so nothing off screen is laid out at all. A 10,000 line file went from 262 ms
  to 7 ms of blocked interface, and the 5,000 line ceiling on the line numbers is gone with
  it. The text lands in a buffer the editor has already sized, which is where most of the
  rest of the time went, and after the first file of a session that buffer is the one the
  previous tab was using, so the text is there in the frame the editor appears in rather
  than the one after. Tabs that are off screen and
  unedited still release their buffer and re-read on the way back, and undo and redo are
  both bounded (they were whole document snapshots, with redo uncapped).
- **A second iced crate is forked.** `vendor/iced_widget` carries two changes. One is a
  one-line fix: iced 0.13's text editor hit-tests a click with the padding applied to swapped
  axes, so an editor with uneven padding puts the caret nowhere near the pointer. The
  editor's line numbers are drawn inside its own left padding, which is what lets a drag
  across them keep selecting instead of stopping at the gutter's edge. The other opens up the
  editor behind a `Content`, so the line numbers can be drawn from the editor's own scroll
  rather than from a copy kept beside it.
- **Syntax colouring for the languages you actually open.** The grammars are bat's set now
  (the `two-face` crate) rather than syntect's own, which shipped none of PowerShell,
  TypeScript, SCSS, LESS, Svelte, F#, TOML, Vue, Dockerfile, Terraform, GraphQL, Kotlin,
  Swift, Zig, Nix or INI. A `.ps1` file was plain text and had a PowerShell icon over it;
  there is now a test that every extension the explorer draws a language icon for resolves to
  a grammar. PowerShell itself is bundled separately (`assets/syntaxes`, MIT): it is one of
  the two grammars bat has that only build against oniguruma, which Arbiter does not use
  because it is C. The bundled Vue and TOML grammars are gone, since the set has both.
  `.csproj`, `.props`, `.targets`, `.resx` and the rest of .NET's XML family colour as XML.
- **A crash now leaves a note behind.** Panics are appended to `panic.log` in Arbiter's data
  folder with the build they came from. A release build on Windows has no console, and
  Windows Error Reporting does not record a Rust panic, so until now a crash took the window
  down leaving nothing at all to read. `ARBITER_OPEN_FILE` opens files in the editor at
  startup, so a fault can be reproduced without driving the UI by hand.
- **Send to Agent.** Select text in the editor and the top item of its right-click menu sends
  it to the Claude running in that workspace, as a fenced block carrying the file's full path
  and line numbers, with the cursor left two lines below it ready for your question. One
  Claude gets it directly; with several, a short list asks which, and with none the same list
  offers every terminal in the workspace. Arrows and Enter or a click choose, Escape cancels.
  Sending hides the editor and focuses the terminal it went to.

- **Agents Office, stage 1: a pixel-art room where one desk is one agent** (`src/agents_office.rs`).
  The room itself, which the entry below wires to the running agents.
  `agents-office-demo` is a standalone harness with fake desks and test buttons
  (`cargo run --bin agents-office-demo --no-default-features`) that drives it without an
  app around it, and is what every judgement about the art was made on.
  The room is drawn, not imported: every pixel is a
  rectangle fill, so the feature adds no asset files, no atlas, no dependency, and touches
  neither the CoreText nor the DirectWrite glyph path. Desks fill five to a row and the
  sixth opens a second row below, so the room grows downward like floors of a building
  while every existing desk keeps its slot; the buffer is only ever blitted at a
  whole-number scale, which steps 4x/3x/2x/1x rather than sliding, because a fractional
  scale gives unevenly sized pixels. A working agent glows in the app's own `AZURE` and a
  blocked one in its `AMBER`, rather than in near-misses of either. Working desks cycle
  through five work poses (typing, reading, note-taking, pondering, a mug of something) on
  per-desk phase offsets, so five agents mid-turn do not look like five copies of one
  sprite; every pose means exactly the same thing, and none of them is a mood. A
  clerestory of windows across the top carries the weather — clear, cloudy, rain, snow,
  fog, and one daylight sky — drawn once however many rows the room has, so the sky costs
  the same at one desk and at twenty; a sunny day spills past the sill but stops above the
  desks, because reading an agent's state must never depend on the weather. A clear night
  has a moon and a couple of thin clouds, and the moon and the sun take opposite sixths of
  the middle window, so changing sky moves the light across the glass rather than swapping
  one disc for another in the same hole. Monitors face
  the camera while the figure beside them stays in profile, so the screen can carry the
  state it is best at: Claude's own spinner turning while a turn is in flight (out of phase
  per desk, so five agents are not five copies of one frame, and paced well below a
  terminal's — this is a picture in the corner of the eye, not a progress bar), output scrolling up under it,
  a highlighted choice waiting when an agent is blocked. A blocked agent also lights the
  andon over its desk: a lamp bolted to the wall above every workstation, dark until it is
  needed, so what catches the eye is a colour rather than an object appearing from nowhere.
  Each occupied desk has a bias light
  behind its monitor washing the wall warm, off at a free desk. Every desk keeps a plant, in one of four
  sizes chosen by slot so a desk's clutter is as stable as its position, and that is all
  the greenery there is: the clerestory and the near floor stay clear so they read as a
  ceiling and a walkway. The figures sit properly: a task chair with a gas post
  and a castor base, a torso of about two head-heights, a thigh running forward under a
  cantilevered desktop whose pedestal is on the far side, and a shin and foot down to the
  floor. The near floor is a shallow walkway with a cable trunk rather than a deep empty
  band of tiles, which read as a missing wall. Four rules are enforced by tests rather than by intent: a desk never
  moves, the working and attention colours are painted by nothing but those two states
  (checked across every sky and every pose), nothing is random (variety comes from a hash,
  so any frame reproduces), and only real activity animates — a turn in flight is the one
  thing in the room that earns a clock, so a room of agents that are idle, or blocked and
  waiting on you, is a single image costing nothing. The andon over a blocked desk is lit
  and steady rather than breathing, because an agent can sit on a permission prompt for
  hours; rain and snow move only as a side effect of a clock some desk had already
  justified, and stop where they are when the last turn ends. Light is structural rather
  than ambient: the room is dark, and what you see you
  see because a particular lamp lit it. A ceiling fixture throws a cone down the wall
  behind each occupied desk and stays dark over a free one, the desk lamp pools on the
  desktop, the screen throws its state colour onto the face turned toward it and onto the
  floor in front of the desk, and every object casts one shadow, at one weight, from one
  key light, which is what glues a figure to a wall and a desk to a floor. The drama is
  all value contrast, so no new hue arrives to compete: the azure and the amber are still
  the only saturated things in the room, and each is now also the brightest thing in its
  own pool of light. None of it moves. The clerestory gets a second, hazier skyline behind
  the near one, since two layers is all the parallax a still picture needs; the walkway is
  the darkest plane, being nearest the viewer and farthest from every lamp; and each desk
  keeps a shelf, a pinboard or a bare wall, fixed by slot, a third of them bare so the
  rest do not read as wallpaper. Every occupied desk carries a nameplate on the floor in
  front of it, the workspace set back over the pane name, drawn into the picture with a
  bundled public-domain bitmap face (Misc-Fixed 6x13, `assets/MiscFixed6x13.bdf`) whose
  printable-ASCII glyphs are compiled into a table: no font file is loaded and no text
  engine runs, so a plate rasterises to the same pixels on Windows and macOS by
  construction. `render_at` enlarges the room first and draws the plates after, at the
  face's own size, so a label stays a fixed height and zooming the window gets you more
  room rather than bigger writing. `Scene::show_names` turns them off and gives the room
  back as pure art. The figures' arms are drawn after the keyboard, because a hand is on
  the keyboard rather than behind it: drawn with the body they used to sit with, the
  keyboard painted over every hand that reached it, so a desk nobody was typing at looked
  like somebody sitting there with no arms.

- **The Agents Office, behind an experimental switch.** Settings, Display, "Show the Agents
  Office" (off by default) puts a desk button in the titlebar and on Ctrl+Shift+G, which
  open a popout room showing every agent that is running, across every workspace. One desk
  is one agent; a pane without Claude in it has none, so quitting Claude frees its desk
  while the pane carries on as a shell. A desk is held for its agent for the life of that
  pane, keyed on the pane rather than the session so a reconnect or a shell switch comes
  back to the same seat, and the room grows a row at a time to twenty. Click a desk to jump
  to that terminal, which now also lifts the main window out of the taskbar if it was
  minimised (the overview's rows and the notification cards behave the same way). The
  window is the art and nothing else: no titlebar, no caption buttons, and the room itself
  is what you drag it by. Turning the setting off closes it, so it can never be left with
  no way to dismiss it. Its clock runs at 12fps and only while some agent is mid-turn,
  inside a window where the app was already drawing, so an open office over a quiet room
  costs nothing at all. A gear in the top corner, faint until the pointer is in the window,
  opens the only menu it has: always on top (off by default, since this is a picture you
  leave in a corner rather than a panel you consult), show names, freeze motion, a position,
  the sky, and Close, which is the only way to shut the window from inside it. The menu
  folds rather than growing, one section at a time, because the sky is seven entries and the
  position nine and a menu carrying both is taller than the window at its smallest. Position
  is nine cells laid out the way the screen is, and it uses the screen the window is already
  on rather than dragging it back to the main display. The desktop is also told not to tile
  the window when it is dragged against an edge: a room drawn at a whole-number zoom has no
  use for half a screen, and it stays freely resizable by its edges. The sky is Auto by
  default, drifting on the room's own clock, which is to say it changes through working
  time and holds still when the room goes quiet: a decoration can never be the reason a
  frame is drawn. Pick any of the six by hand and that turns Auto off. The desk under
  the pointer takes the floor marker, so you can see which agent you are about to click
  without anything having to light up in a colour the room has reserved.

### Changed
- **Scrollbars show only while you are over them.** The file explorer, Settings, the
  shortcuts sheet, the Overview list and the dialogs drew iced's default scrollbar, a
  grey track with a thumb, all the time. They now match the terminal's: a thin rounded
  thumb and no track, hidden until the pointer is over the list, a little brighter on
  the bar itself or while dragging it.

### Fixed
- **A window saved on a monitor that is not connected opens on one that is.** Quitting
  at home with Arbiter on an external monitor and starting it at work put the window
  where that monitor had been, out of reach. At startup each window's saved position is
  checked against the displays there are now: one whose title bar is on none of them
  opens centred on the primary display, and one too big for its display is shrunk to
  fit. On macOS and Windows alike.
- **A shell Claude left running in the background keeps the pane working.** A turn that
  ends with a background shell still going (`✻ Brewed for 14m 1s · done 11:40 PM · 1 shell
  still running`) looked finished: the spinner stopped, the dot went idle and "Claude
  finished" fired, although Claude picks the work up again by itself the moment the shell
  exits. It is now held exactly as a wait on background agents already was, working until
  the line goes and with no card at the turn end. One honest cost: Claude draws the same
  line for a dev server it started in the background, which never exits, so that pane
  stays working for as long as the server runs.
- **A chooser you opened yourself is not Claude asking for something.** Typing `/model`,
  `/config` or any other slash command raised a "Claude needs your input" card. Claude
  lists its commands on the first `/`, and that list carries the footer a real prompt
  carries, which no hook distinguishes. The pane now reads its own input box, so the list
  is quiet as you type and so is the chooser Enter opens. A command that sets Claude
  working, like `/init`, still reports whatever that turn asks for.
- **Scrolling back over a plan no longer raises cards.** The scan that finds a prompt read
  the whole screen, so an approval box scrolled back into view looked like a live one, and
  every pass over it raised another card. A prompt only counts at the live bottom, within
  the rows Claude draws its input box in, and a footer now has to look like a footer
  rather than merely contain one of its phrases.
- **"Claude finished" now means Claude finished.** The turn end was inferred from two
  seconds of silence in the spinner animation, so anything that interrupted it for that
  long raised a card: dragging the explorer edge or a split divider, toggling the editor,
  switching workspace, a frozen status row after scrolling, a narrow pane wrapping a
  status line. A turn end is now the event that ends it, either Claude's Stop hook or its
  "esc to interrupt" hint giving way to its idle input box and staying gone for a moment,
  and nothing else can invent one. That hint is also read directly, so a status row that
  has frozen or a read that has stalled no longer decays into "finished". A turn you stop
  yourself with Esc or Ctrl+C raises nothing, and one that stops on a question raises
  "Claude needs your input", not "Claude finished".
- **Waiting for background agents survives scrolling.** Scrolling up during the wait ended
  the hold added in 1.5.1 and raised a card about two seconds later.
- **The usage refresh keeps the numbers on screen.** Clicking refresh flashed "Usage
  unavailable": it reloaded the whole claude.ai page, and a failure during that cold load
  was parsed into a state that discarded every meter. A refresh now asks the live page to
  refetch, which is far quicker, and falls back to a reload only if that goes unanswered.
  Figures that fail to refresh stay on screen instead of being replaced by a warning, and
  the arrow turns while a refresh you asked for is in flight. An automatic refresh still
  draws nothing at all.

## [1.5.1] - 2026-09-21

### Added
- **A split opens where you are.** A terminal opened by a split starts in the directory of
  the terminal you split, on Windows and macOS alike (Settings, General, "Split into the
  same directory"; on by default). A terminal inside `ssh` hands on the local directory it
  left from.
- **Ctrl+Shift+P shows a test notification** about the focused terminal, cards enabled or
  not, so a card can be looked at and the chime heard without waiting for Claude.

### Fixed
- **Waiting for background agents no longer reads as idle.** While Claude's status row says
  "Waiting for N background agents to finish" the pane stays working and no "Claude
  finished" card is raised. Claude's turn is over by every other sign (the spinner stands
  still and the Stop hook has fired), yet it resumes on its own when the agents report.
- **A notification card no longer takes the keyboard on macOS.** iced shows every window it
  opens through winit's `set_visible`, which on macOS is `makeKeyAndOrderFront` whatever
  the window's inactive flag said, so the card became the key window and the terminal
  stopped receiving keystrokes. The vendored runtime now lets such a window be shown by its
  creation, where winit honours the flag.
- **A card's text and bell are centred vertically.** The row hugged the top padding and
  left the slack below.
- **Memory no longer grows with the panes you have closed.** Every terminal keeps a GPU
  renderer (its glyph atlases, some 6 MB) in a store the app never pruned, so a pane closed
  or reconnected left its renderer behind for the life of the app. A two-day session held
  30 of them for 18 panes. A dropped session now retires its id and the store frees the
  renderer on the next frame; the Claude monitor thread each closed pane also left blocked
  ends with it.
- **A frame is rebuilt only when something on it changed.** Output in any pane re-walked
  every visible terminal's grid and re-uploaded its whole instance buffer, at 60 fps while
  Claude worked, for panes whose contents were identical frame to frame. The terminal now
  carries a generation the renderer compares before doing either. The same traffic drove
  the GPU upload heaps from 100 MB to over 300 MB in two days; they now see a fraction of it.
- **A new glyph uploads its own atlas slot, not the whole atlas.** Each first appearance of
  a character re-sent the 1 MB mono atlas or the 4 MB colour atlas; now only its rectangle
  goes up. The renderers also share the bundled font bytes instead of each holding a copy.
- **A full glyph atlas starts over instead of indexing past its end.** Enough distinct
  glyphs in one pane (thousands; CJK or icon-heavy output over weeks) would have panicked
  the renderer. The atlas now flushes and the glyphs in use re-rasterise.
- **Output bursts no longer spawn a thread each.** The held redraw, the frozen frame and the
  hidden cursor's grace each started a thread per event, eight or more a second under
  Claude output. One deadline thread now serves them all, parked with no timer while
  nothing is armed.
- **Scrollback is capped while Claude owns a pane.** Claude takes the wheel there (mouse
  reporting), so the pane's history is unreachable, yet Claude's redraws filled it to the
  configured 5000 lines, some 25 MB per pane. It is held at 1000 lines for the duration
  and returns to the setting when Claude leaves.
- **Only one graphics backend loads on Windows.** iced let wgpu initialise every backend it
  was built with, DX12 and OpenGL beside the Vulkan it drew with, tens of MB of driver for
  nothing. The app now probes for a Vulkan adapter before starting and asks for Vulkan
  alone. A machine without one gets DX12 and a notice saying so: DX12 draws an unmaximised
  window soft, because DXGI stretches the frame to the window and winit's borderless-shadow
  hack leaves the client rect one row taller than the window shows, so the frame is squeezed
  by a pixel. A DX12-only build was tried first and had every window that way.
  `WGPU_BACKEND` set by hand still wins.
- **`exit` closes the terminal.** A shell that ends with exit code 0 (`exit`, Ctrl+D)
  closes its pane as Ctrl+Shift+W would; the last pane of a workspace closes the
  workspace, unless it is the only one. A shell that dies otherwise keeps its screen and
  offers Reconnect, as before. Detected by a per-session exit watcher, since ConPTY gives
  the reader no EOF when the child ends.

### Added
- **`ARBITER_MEM_DIAG=1`** logs every allocation of 8 MiB or more with a backtrace, and a
  summary of the live large blocks once a minute, to `<temp>/arbiter-mem-diag.log`. Off,
  it costs one atomic load per allocation.

## [1.5.0] — 2026-09-18

### Added
- **A Fable usage meter in the titlebar, on request.** Plans that cap Fable separately
  report that cap as a scoped weekly limit rather than a window of its own, so the usage
  bars never showed it. Settings → Claude usage → "Show Fable usage" (off by default) adds
  a purple Fable bar left of the 5h bar. It resets together with the 7d bar and so carries
  no timer. The overview footer is unchanged.

## [1.4.3] — 2026-09-18

### Fixed
- **Scrolling a working Claude's transcript no longer freezes its spinner, and Arbiter's
  working bar with it.** Claude Code's fullscreen UI (2.1.27x) stops animating its status
  row once the row has scrolled out of view, and scrolling back does not resume it; only
  a change of the row's own text, a focus event or a resize re-renders it
  ([anthropics/claude-code#94443](https://github.com/anthropics/claude-code/issues/94443)).
  With no spinner frames arriving, Arbiter read the turn as over two seconds later and
  took the working bar down too, until Claude next printed something. After a wheel
  gesture handed to Claude, Arbiter now watches whether the row keeps drawing and, while
  it has stopped, nudges Claude into the re-render it needs: a focus pulse, and if that
  changes nothing (it does not on Windows, where node never sees console focus events), a
  resize of the PTY by one column and back. The pane keeps its frame while Claude repaints
  for the nudge, so the screen does not flicker; it simply resumes. Only a pane running
  Claude is ever nudged, and never while its transcript is scrolled away from the bottom.
- **Scrolling up during a turn no longer ends it in Arbiter's eyes.** While Claude's
  transcript is scrolled away from its live bottom (its "Jump to bottom (ctrl+End)" bar is
  showing), Claude draws no spinner frames at all, so two seconds later Arbiter took the
  turn for over: the working bar went out and a "Claude finished" card appeared, sound and
  all, mid-turn. A turn that was live when the scroll began is now held as working until
  the view comes back, where it gets a fresh window to resume in. An idle Claude scrolled
  away stays idle.
- **A pane keeps drawing under output that never pauses.** Since 1.3.0 a redraw waits for a
  burst of output to settle before the terminal is rebuilt from the grid, so an update
  arriving in several chunks is drawn whole. Output arriving faster than the settle time,
  chunk after chunk, re-armed that wait indefinitely and could keep one frame up until the
  output paused. A frame is now kept at most as long as a redraw is ever held (40 ms).
- **The usage webview no longer navigates itself to death (Windows).** The script
  Arbiter injects into claude.ai to read usage sends any frame that isn't claude.ai
  back to claude.ai, a rescue meant for the page itself. WebView2 runs injected
  scripts in every frame, not just the top one (macOS and Linux inject into the main
  frame only), so each of claude.ai's own subframes was navigated to claude.ai, and
  the subframes of those in turn: a page load every couple of seconds, around the
  clock, for as long as Arbiter ran. A few hours of that exhausted the renderer's
  memory, WebView2 crashed and wrote a 20 MB dump, and the usage bars died until the
  app restarted. The script now runs in the top frame only.
- **A silent usage webview is restarted instead of being left for dead.** The
  background poll escalated an unanswered refresh to a page reload and stopped there,
  which reaches nothing once the webview's browser process is gone. It now takes one
  more step after two silent cycles and replaces the helper process, so the bars come
  back on their own within a few minutes rather than at the next launch. A helper that
  exits for any other reason is likewise respawned, unless three runs in a row produce
  nothing (no webview to be had on this machine), when it stops trying.

## [1.4.2] — 2026-09-14

### Added
- **Notifications.** When Claude in a terminal finishes a turn or stops to ask for
  input, a card appears in the lower right corner of the main monitor (its start-up, and
  a session being resumed, read as work too but announce nothing):
  above every other window, shown without taking the keyboard from wherever you are
  typing, naming the terminal and its workspace. It leaves after a few seconds, or when
  clicked, which selects that terminal and brings Arbiter to the front, out of the
  taskbar if need be; up to four stack upward with the newest nearest the corner. Each
  one comes with a short, soft sound bundled with the app, played through the OS (winmm on
  Windows, `afplay` on macOS). While a game or video runs full
  screen, or a presentation, only the chime plays: no card is put over it (Windows' own
  rule for its toasts; a macOS fullscreen app has its own Space, which the card does not
  enter). Off by default: the Notifications tab in Settings turns them on, and there
  chooses which of the two events to announce and whether the sound plays.
- **The overview shows a waiting Claude in more than a dot.** Its row is washed and edged
  in the dot's amber.

### Fixed
- **`arbiter` typed in a pane of the installed app draws the about screen again.** The
  Windows release exe is a GUI program with no console of its own, so `about` and
  `--version` had nowhere to print: the text was dropped, and the `claude --version` probe
  behind one of the facts flashed an empty console window instead, which is all a user saw.
  Both now attach to the terminal that launched them, and the probe runs without a window.
  Debug builds, and any run with output redirected to a file, were never affected.

## [1.4.1] — 2026-09-14

### Added
- **Panes announce themselves as Arbiter to the far end of an ssh session.** Every pane
  now carries `LC_TERMINAL=Arbiter` next to its pane id, the variable iTerm2 uses for the
  same purpose and for the same reason: sshd accepts `LC_*` by default (macOS, Debian
  family), so with `SendEnv LC_TERMINAL` under `Host *` in the client's `~/.ssh/config` the
  remote shell can tell its output is drawn by Arbiter. A remote Claude status line can
  then emit Nerd Font icons, which the bundled symbols font renders, instead of the plain
  fallback it uses when no Nerd Font is installed on that host.

### Fixed
- **Icons are drawn at a readable size and sit on the baseline.** An icon squeezed into
  one cell had been scaled down to the cell's width, smaller than a capital letter. When
  the cell after an icon is blank with the same background, as it is after every icon in
  a prompt or status line, the icon now takes both cells, as Windows Terminal and WezTerm
  let icons overflow into the following space: drawn at four fifths of its natural size,
  about the height of a capital, and seated on the baseline rather than hanging under it,
  with part of the borrowed blank left as a gap before the text. An icon followed directly
  by text keeps one cell as before; Powerline separators never take a second cell.
- **Icons that keep one cell are no longer cut in half on macOS.** An icon followed
  directly by text, and every Powerline separator, keeps a single cell, where the bundled
  symbols font draws it at nearly the full em: about twice the width of a Menlo cell. Only
  Windows was scaling such a glyph into its cell, so on macOS the blit clipped it and half
  the icon was missing. Both platforms now fit it, while ordinary text on macOS is still
  left alone (pixel rounding puts an `M`'s ink a hair past the cell, and rescaling that
  would mangle normal text).
- **Powerline separators fill their cell.** Where the terminal font lacks them (the
  installed Cascadia Mono and Menlo both do), the separators came from the bundled symbols
  font and were fitted into the cell keeping their shape, which left a notch of about a
  third of the cell above and below each one. The four straight separators (U+E0B0 to
  U+E0B3) are now drawn by the renderer at exact cell size, like box drawing, and the rest
  of the range (rounded, slanted, flame and the others up to U+E0D7) is stretched to the
  cell on each axis, the way a patched Nerd Font sizes them.
- **Clicks, drags and wheel scrolling land on the row under the cursor.** The mouse hit
  test divided the pane's height by its row count, but the grid leaves a blank strip below
  the last row, so every row boundary sat too low, by nearly a full row at the bottom of a
  tall pane. It now uses the cell size the renderer actually drew with.

## [1.4.0] — 2026-09-13

### Added
- **Icons render out of the box.** The icons a prompt (starship, oh-my-posh), a status
  line or `eza --icons` draws live in Unicode's Private Use Area, where no system font has
  glyphs, so every terminal shows them as small boxes until you install a Nerd Font and set
  it as the terminal font. Arbiter now bundles the icons-only "Symbols Nerd Font Mono" (as
  WezTerm does) and takes exactly those glyphs from it when the terminal font lacks them,
  before the OS's own fallback. Nothing to install, nothing to set, and the terminal font
  stays what it is; Claude's own symbols (⏺, ✳) render as before. Licence in
  `assets/SymbolsNerdFont-LICENSE.txt` (MIT).

- **Wake a machine over the network.** Settings has a Wake on LAN tab: add a machine by
  name and MAC address (any of the usual spellings), remove it again, and switch on a Wake
  button in the titlebar, right of the overview button (off by default). The button, or
  Ctrl+Shift+M from anywhere, drops down the list; click a machine, or move with the arrow
  keys and press Enter or Space, and its magic packet goes out as a broadcast on the local
  network. A checkmark grows in on the row and the menu closes a moment later; a send the
  network stack refuses says so on the row instead. Reaches machines on the same LAN or
  Wi-Fi with Wake-on-LAN enabled in their firmware and adapter.

## [1.3.0] — 2026-09-13

### Added
- **Attaching a file to a terminal on another machine now works.** Drop a file on a
  remote terminal, or attach a screenshot with Ctrl+Shift+S, and Arbiter copies it to the
  far host first, then pastes the copy's path, so Claude there reads it as if it had been
  dropped locally. The copy goes over a second connection made with `sftp` from the
  terminal's own ssh line, into `~/.arbiter/attach` on the far host, and takes well under
  a second on a LAN. A password or key passphrase is answered from what was typed into
  the sign-in dialog this run; a connection not signed in through Arbiter asks once, in
  that same dialog. Copies older than a day are removed by Arbiter itself, once a day per
  host, after the day's first copy. Works from Windows or macOS to a Mac, a Linux box or a
  Windows OpenSSH server alike, since sftp needs nothing of the far host's shell. While a
  copy is under way the terminal's header says "Copying to mini…" with a pulsing upload
  arrow, since the path lands only when it is done and typing meanwhile still goes through.

### Fixed
- **The cursor no longer flickers while Claude works over ssh.** Two causes, both fixed.
  Claude's UI hides the cursor while it redraws and shows it again afterwards, and over
  ssh, at the pace of its working animation, the hide and the show landed in separate
  frames; the cursor now stays drawn for a quarter second after a program hides it, long
  enough to bridge that, while a program that means to hide its cursor still loses it.
  And a screen update that leaves the far host as one write arrives over ssh in several
  pieces, which the frames drawn in between showed half-done (the input line erased and
  not yet redrawn), all the more while the working animation's repaint clock runs; a
  frame is now built from the grid only once output has paused for 10 ms (never more
  than 40 ms in a row, so a scrolling log still moves), and the previous frame stays up
  in between. Locally an update arrives whole, so nothing changes there.
- **A small black box no longer appears in the top-left corner of the primary monitor
  on Windows.** Arbiter asks Windows 11 to round the corners of its windows and did so
  for every window of its process, including the invisible 16x16 message window winit
  keeps at the screen origin for its event loop. Rounding a transparent layered window
  makes DWM paint a frame for it, which is what the box was. Utility windows are now left
  alone.

## [1.2.0] — 2026-09-12

### Added
- **`arbiter` in any terminal shows the mark and the build.** Type `arbiter` in a pane and
  the two strokes of the logo paint in, in its own blue gradients, followed by the version,
  the commit and date it was built from (with a `+` when the tree was dirty), the profile,
  target and compiler, the OS and host, whether the terminal is an Arbiter pane, the shell,
  the saved layout (workspaces, terminals, how many over ssh), the data directory and the
  Claude version. Plain text and no animation when piped or with `NO_COLOR`. Outside a pane
  the same is `arbiter about`, and `arbiter --version` prints one line.

- **The sign-in dialog says which secret each connection wants.** A row now carries a
  gold key and "Key passphrase for id_ed25519", or a blue lock and "Password for
  tre@10.0.0.16", read from ssh's own prompt the first time it appeared, and the field's
  placeholder says "Passphrase" or "Password" to match. A connection that has never
  prompted still reads "Passphrase or password" until it has.

- **Switch a connection off in the sign-in dialog.** Each row ends in a "Connect" switch:
  off, that connection's terminals are left at their local prompt instead of connecting
  (its field greys out), while the others connect as usual. A skipped terminal keeps its amber
  Reconnect button; pressing it
  asks for that connection's secret once and connects that terminal only, and the secret is
  then remembered for the other terminals on the same connection, which connect with a
  press each. The button stays until you run a command in the terminal, which makes it an
  ordinary local one. A skipped connection that is never brought back is saved as local, so
  it is not offered again on the next launch.

- **A restored SSH terminal can resume its own Claude conversation, not just the
  directory's most recent one.** Local terminals always did this, because Arbiter's shim
  learns each Claude's session id from its status line; nothing on a far host can tell
  Arbiter that, so Arbiter can name the conversation itself. With **Name remote Claude
  sessions** on (Settings, General; off by default), typing `claude` at the far prompt and
  pressing Enter completes the line with `--session-id <uuid>` before it goes through, and
  you see the argument appear. A restore or reconnect then runs `claude --resume <id>`,
  falling back to `-c` and then a fresh `claude`, so several terminals in one remote
  directory each get their own conversation back. Nothing is installed on the host.

  It is off by default because it visibly edits what you typed, which is a surprise to
  anyone who did not ask for it. The completion only happens for a plain `claude` (flags
  allowed, no subcommand, `-c`, `-p` or picker) at a prompt ending a shell uses (`$`, `%`,
  `#`), which Claude's own input box never shows. A `claude --resume <id>` you type is read
  as it is whether the setting is on or not. Should Claude report the conversation gone,
  the id is forgotten and the fallbacks take over.

## [1.1.0] — 2026-09-12

### Added
- **One sign-in prompt for all restored SSH terminals.** Restoring several SSH terminals
  used to mean typing the same passphrase separately in each one, blind, with no echo to
  confirm you had typed it right. Arbiter now asks once, before anything connects, with a
  masked field per connection, and each terminal answers its own prompt.

  Terminals sharing a connection share a row: five terminals on one host is one field and
  one answer. A row left blank connects and asks for itself in its own terminal, exactly as
  before, and "Connect without" (or Escape) does that for everything at once.

  Asking **before** connecting is deliberate rather than incidental. A connection left
  sitting on an unanswered prompt is burning the server's login grace period (`sshd`'s
  `LoginGraceTime`, two minutes by default) and counting against its limit on
  unauthenticated connections, so a slow answer could have dropped the very connections it
  was meant to establish. Nothing connects until the dialog is answered, so neither limit
  is approached however long it stays open or however many terminals are restoring.

  **Never written to disk.** A secret is kept in memory for as long as Arbiter runs, one
  per connection, so a connection that drops (a timeout, a lost link, a laptop waking up)
  comes back with at most a button press and no typing. It is never written to
  `session.json` and never logged, and it is gone when Arbiter quits, which will ask again.
  The field is a real masked input, so copy and cut are disabled on it too. A terminal
  answers one prompt per connection attempt; if ssh asks again or refuses, the secret is
  dropped and a one-row dialog asks for it afresh instead of retyping it. A held secret is
  also let go the moment the login is seen to complete, or after 90 seconds, so a later
  password prompt on the far host (a nested `ssh`, a `git push`) can never receive it.

  The dialog only lists connections that have asked before. Once a connection has been seen
  to log in without a prompt (a key in an agent, a key without a passphrase) it is left out,
  and one that has never been seen is listed until it is. With nothing to ask, nothing
  waits: the terminals connect at once.

- **A dropped SSH session reconnects itself, once.** When ssh exits with its own failure
  code (255: a timeout, a reset link, a host that went to sleep) after a session that was
  up, Arbiter re-runs the connection in the same terminal, answering the prompt from
  memory, so a brief outage costs nothing. If that attempt fails too, the amber Reconnect
  button takes over, and clicking it now needs no typing either. A deliberate `exit` on the
  far host exits cleanly and is never reconnected; it also makes the terminal an ordinary
  local one again, with no Reconnect button and nothing to replay or sign in to on the
  next launch, until the next `ssh` typed there. An attempt that never reaches the host
  (refused, timed out, unresolvable) is a refusal, not a drop, however long ssh waited on
  it, so a host that is down is not hammered while it stays down. Reconnect reuses the live
  local shell when it is sitting at its prompt, so the scrollback survives; only a shell
  that has exited, or is busy with something else, is respawned. The button also appears
  when a restored connection fails before it is even up, which it used not to.

  The local shell integration now reports each command's exit status (`OSC 133;D;<code>`),
  which is what tells a drop from an `exit`.

- **A restored SSH terminal comes back in its remote directory, with its Claude.** The
  connection itself carries the directory: a saved `ssh host` is replayed as
  `ssh -t host 'cd <dir> && exec $SHELL -l'`, so the terminal opens there with nothing typed
  after connecting (ssh has no option for a starting directory; a remote command is the
  only handle on it). A directory that no longer exists ends that connection with the
  `cd`'s own status, which Arbiter reads as "gone" and forgets, so the next attempt lands at
  home instead of failing the same way. For `mosh`, `plink`, or an ssh line with quoting or
  a remote command of its own, the `cd` is typed at the far prompt instead.

  If Claude was running there when the layout was saved or the connection dropped, it
  rides in the same line: `claude -c` continues the most recent conversation in that
  directory, or a plain `claude` starts if there is none, which is the rule local
  terminals already follow, and the login shell takes over when Claude exits. It runs
  inside an interactive login shell (`exec $SHELL -lic …`) because the shell sshd hands a
  remote command to reads no rc file and so has no `claude` on its PATH; that inner
  command travels as one backslash-escaped word, since no quote survives the trip through
  every local shell, which is why the line looks the way it does. Claude is recognised on
  the far side by its screen, and also by `claude` having been typed at the far prompt
  with nothing else typed at a shell prompt since, so a Claude whose interface is not
  recognised still comes back. Two terminals on the same connection and directory would
  fight over one conversation, so only the first resumes; resuming the exact conversation
  the way local terminals do would need the far Claude to say which it is, and is left for
  later (the design is noted in CLAUDE.md).

  Where the far shell is comes from one of two places. Without anything installed
  remotely, Arbiter follows the `cd` commands you type there, read from the screen as the
  shell showed them (so completed and recalled paths count), and takes back a `cd` the
  shell rejected. A `cd` it cannot read (a variable, a glob, `cd -`) makes the directory
  unknown rather than wrong, and the terminal then lands at the remote home with no Claude.
  For an exact answer, add a small snippet to the far host's `.bashrc` or `.zshrc`
  (right-click a remote terminal: Copy Remote Directory Snippet). It reports the working
  directory with the standard OSC 7 sequence at every prompt and nothing else, the menu
  item reads "(active here)" once reports arrive, and its reports override anything
  inferred.

  Where the line cannot be rewritten (`mosh`, `plink`, an ssh line with quoting or a
  remote command of its own) the `cd` and Claude are typed at the far prompt instead, and
  only once the far shell is known to be at it (its own report, or a prompt recognised on
  screen after any credential prompt has been answered), so a credential prompt can never
  receive them.

- **Claude running over SSH is now detected.** Until now every Claude signal came from the
  local machine: a scan of the pane shell's child processes, plus the status-line capture
  file Claude writes through Arbiter's shim. Neither can see a `claude` running on another
  host, so an SSH pane showed no status dot, was filtered out of the overview when "Claude
  only" was on, and got the wrong Shift+Enter key encoding (submitting instead of inserting
  a newline).

  A pane whose foreground program is an `ssh`/`mosh` client is now recognised on the same
  busy edge that already scans for Claude, and such panes get a second detector that looks
  for Claude's own interface in the rendered screen. Those markers arrive as ordinary
  terminal output, so they work over any connection with **nothing installed on the remote
  host**. The scan is anchored to the cursor, where Claude draws its input box: that finds
  it whether it has just started or has scrolled a long transcript, and once Claude exits
  its last screen is left above the returning prompt, where the scan does not reach, so the
  pane clears itself. A turn in flight holds the pane's state even though Claude swaps its
  hint line for the interrupt hint while working.

  **Local panes are untouched.** The screen probe runs only on panes with an SSH client, so
  the local path is exactly the one that already worked. What Arbiter *persists* also stays
  local-only, so a restored SSH pane no longer tries to start a local Claude.

  Set `ARBITER_CLAUDE_DEBUG=1` to log the probe against local panes, where the process scan
  gives ground truth to compare against. That is how to check the markers still match if a
  future Claude release changes its interface.

- **SSH terminals come back on restart, and can be reconnected.** A remote terminal now
  remembers the command that built it and replays it on relaunch, so a workspace full of
  SSH panes reopens connected to the right hosts instead of at local prompts. The command
  is stored as the literal line you typed, which is what makes it work for any host, jump
  chain or wrapper script without Arbiter needing to understand connections.

  Without the remote directory snippet (see above) it stops there: the pane lands at the
  remote prompt and you start Claude. Nothing has to discover or guess a session id on the
  far host, so there is nothing that can go stale. Local terminals are
  unchanged, still restoring by respawning their shell in the saved directory, and never
  replay a command, since re-running an arbitrary last command on every launch could have
  side effects.

  Only plainly typed commands are remembered. Recalling one with the up-arrow, completing
  it with Tab, or interrupting it abandons tracking, so the pane restores to a plain shell
  rather than replaying something you did not run.

- **A terminal whose shell exits now says so, and offers to come back.** Previously the
  pane sat on a frozen screen with no sign anything had happened, and typing into it did
  nothing at all, silently. A dropped SSH connection or a slept remote host now puts an
  amber Reconnect button in the terminal's header, which brings it back and replays its
  startup command, keeping its name, position and command history. There is also a
  Reconnect entry in the right-click menu, enabled for remote terminals and any whose
  shell has exited.

### Fixed
- **Claude's bullets and one spinner frame no longer render as coloured squares on
  Windows.** Claude on a Mac or Linux host draws its tool bullets with `⏺` and one spinner
  frame with `✳`, characters Claude on Windows never uses, so this only showed over SSH.
  Both are emoji-capable, and DirectWrite's own font fallback reaches for Segoe UI Emoji
  in their blocks, which returned the colour button glyph (a blue square, a teal square)
  squashed into the cell. A character in a single-width cell has text presentation by
  definition, so such a character the terminal font lacks is now laid out in Segoe UI
  Symbol by name, giving the plain glyph in the text colour, the way every other terminal
  shows them. Double-width emoji are untouched, and macOS rendering is not involved.

- **A remote shell's directory reports no longer overwrite the local terminal's.** A far
  host that already emitted OSC 7 at its prompt (fish, vte.sh, a custom prompt) was taken
  for the local shell: the pane's folder, git status and saved directory followed the remote
  path, on Windows mangled into `home\tre\src`, which Reconnect then handed to the new shell
  as its start directory. Reports are now told apart by the host name they carry, remote
  ones feed the remote-directory feature above, and Reconnect never starts a shell in a
  directory that does not exist.

- **A restored SSH terminal no longer falls back to password authentication.** On relaunch
  the replayed `ssh` command would show its key passphrase prompt and then immediately
  give up on the key, asking for the account password instead. Typing the same command by
  hand a moment later worked fine, which is what made it so confusing.

  Two things had to coincide. Terminals are created at 80x24 and resized to their real size
  on their first rendered frame, which lands *after* the shell has printed its prompt, so
  the replayed command ran during that resize. A resize arrives as a window-size change to
  whatever is running, and Git's MSYS build of `ssh` does not survive one during its
  passphrase read: the read is abandoned, it reports an incorrect passphrase, and `ssh`
  moves on to the next authentication method. Native Windows OpenSSH is unaffected, which
  is why this only appeared when Arbiter was started from Git Bash, whose `PATH` puts
  Git's `ssh` ahead of the Windows one.

  A replayed command now waits for the terminal to be both readable (its shell has
  prompted) and at its real size before it is sent. Both arrive as events, so there is no
  polling and no timing guess; whichever happens last releases the command.
- **An SSH terminal no longer shows a permanently green "running" dot.** That dot comes
  from the local shell's integration, and from its point of view `ssh` is a single command
  that runs from connect to disconnect, so the dot lit up the moment you connected and
  stayed lit until you left, saying nothing about the remote host along the way. Remote
  panes now leave it to Claude's own status, which is read from the screen.
- **An SSH terminal no longer reports the wrong git status.** The overview's git counts come
  from the pane's local working directory, which for an SSH pane is just wherever the shell
  happened to be standing when `ssh` was typed, so it described an unrelated repo on your
  own machine. Remote panes now show no counts rather than misleading ones, and the row's
  title gets the freed space.
- **Shift+Enter inserts a newline in a remote Claude** instead of submitting. The key
  encoding is chosen by whether Claude is running in the pane, which now includes panes
  where it is running over SSH.

### Removed
- **The per-terminal stats footer is retired,** along with the in-pane info card (the ⓘ
  button in a terminal's header). Both restated what Claude's own status line already shows
  inside the terminal: model, context percentage, and token counts appeared twice, one line
  apart. Claude's version also has the advantage of being portable, rendering the same over
  SSH, where Arbiter's footer could only ever show stale local values. Each terminal gains
  back the footer's 26px.

  The folder segment's "rename this terminal to its repo name" action was the footer's one
  unique feature, so it moves to the terminal right-click menu as **Rename to Repo Name**,
  enabled only inside a git repo.

  Per-session cost, previously only in the info card, is no longer shown. Claude hands its
  status line the same `total_cost_usd` Arbiter was reading, so adding it there brings it
  back and works over SSH too.

  Downstream, this makes the whole token/context/cost pipeline dead: nine fields of the
  per-pane status and eight of the statusLine capture are gone, leaving the capture with
  just the session id. The capture files themselves stay, since their existence is how a
  pane knows Claude launched and how the attention/turn-end hooks are routed.

  **The overview popout is unchanged** and still shows every terminal's live status dot and
  git branch.
- **Project workspaces are retired.** Every workspace is now a plain terminal workspace
  (tabs of terminals). This removes the git-worktree sidebar (with its robot avatars, merge
  / discard / remove actions and the new-worktree dialog), the file-explorer sidebar (with
  its right-click menu, rename and delete dialogs, and per-file-type icons), and the "+"
  dropdown, which had only one remaining choice and so now creates a terminal workspace
  directly. Roughly 2,100 lines lighter.

  Existing saved sessions keep loading: the `project` data in an older `session.json` is
  simply ignored, and each workspace restores the terminals its active worktree had, with
  its name, split layout and per-terminal history intact. Terminals that were open in a
  *non-active* worktree are not restored, since there is no longer anywhere to put them.

  The overview popout, per-terminal Claude status dots and the titlebar usage bars are
  unaffected. The main restore path is now a single shape rather than two, which is
  groundwork for the SSH work that follows.

### Added
- **Terminal font size setting (Settings → Display → Terminal).** Pick a point size
  (8–32); it applies to every open terminal immediately. The renderer rebuilds at the new
  size and each terminal reflows its columns/rows, so cell alignment and box-drawing stay
  exact. Persists across restarts.
- **Open Claude config from Settings (Settings → Claude Usage → Config).** Buttons that open
  Claude's `settings.json` and `~/.claude.json` in your default editor (resolving
  `$CLAUDE_CONFIG_DIR` / `~/.claude`), on macOS and Windows. A missing file is created empty
  so the button always opens something.
- **Hiding usage now hides it everywhere.** The overview usage footer is hidden whenever
  usage is hidden globally (the same "Hide usage bar" setting the titlebar × toggles). The
  overview's own "Show usage footer" toggle still hides just the footer independently when
  usage is shown.
- **Dismiss & explain the titlebar usage sign-in.** The header "Claude Usage Sign In" prompt
  now has a small × to hide it outright (the same "Hide usage bar" setting, persisted).
  Clicking Sign in first shows a short, plain-language explanation of what signing in does
  and what data is read, with Sign in / Cancel. Cancel leaves the header as-is.

### Fixed
- **"Usage unavailable" now recovers on its own, and clicking it retries.** This state means
  you're still signed in but the usage fetch failed transiently (network blip, a 5xx/429 from
  claude.ai, or the hidden WebView2 renderer being discarded), which is why opening it showed a
  working Claude chat. Previously it was a dead end: the error state had no refresh button and
  the 120s background poll paused (it only ran while usage was `Ok`), so nothing retried.
  Clicking the "Usage unavailable" pill now retries (reloads the helper → respawns the renderer
  + refetches) instead of opening the sign-in webview, showing "Loading…" as feedback; if the
  reload reveals a genuine logout it flips to Sign in. The background poll also keeps reloading
  while in the error state until it recovers. (Settings → "Reconnect" still opens the webview
  for the heavier re-auth path.)
- **Scrolling a Claude pane no longer shows it as "working".** Claude Code enables mouse
  reporting, so a wheel notch scrolls *its* transcript: every notch makes it redraw the whole
  screen, re-emitting whichever "✻ Brewed for 7s" thinking summaries are on it. Those repeats
  paired into a false working state (azure bar and dot, plus the 60fps clock they force) for
  as long as you kept scrolling. Entering "working" now also requires the paired spinner frames
  to draw *different* glyphs, which the ✻ bloom does every frame and a repaint of the same
  static star never does; a wheel notch handed to a mouse-reporting pane additionally holds off
  detection for 300ms. Scrolling during a turn that really is working still keeps it working:
  suppression only blocks *entering* the state, never sustaining it. Most visible on Windows,
  where every wheel notch reaches the app (a macOS trackpad's pixel deltas usually round to no
  notch at all).

## [1.0.12] — 2026-06-23

### Fixed
- **Ctrl+C returns to the prompt:** pressing Ctrl+C (the `^C` interrupt, when there's no
  selection) while scrolled up in the scrollback now snaps the view back to the live bottom,
  matching what typing already did — so an interrupt jumps you to the prompt.
- **Overview Claude avatar alignment:** the static Claude starburst shown left of a terminal's
  title in the overview is raised 1px so it sits level with the title.

## [1.0.11] — 2026-06-23

### Changed
- **True idle when idle (lower CPU/GPU; lets the display sleep while focused).** The UI
  no longer runs on a repaint clock when nothing is animating. Previously even an idle
  window repainted ~1×/sec, which kept the wgpu swapchain presenting; on Windows the GPU
  driver responds to continuous presentation by raising the global timer resolution to
  1 ms (defeating CPU power management) and keeping the display/GPU awake — so a focused
  window never let the machine idle or sleep. Now, when idle, the app emits **zero
  frames** and repaints only on real events (terminal output, input, window events); the
  swapchain goes quiet, the driver drops the raised timer, and Windows can sleep the
  display even while Arbiter is focused. A pane **waiting at a prompt** (`Attention`) is
  now a **solid** amber dot instead of a pulsing one — a waiting prompt is idle, so it no
  longer pins the UI at 60fps (the overview's green "running" dot is likewise solid now,
  matching the tab/header). The usage auto-refresh no longer rides a UI tick at all: its
  120s cadence runs on a background thread that pokes the helper directly, so a logged-in
  idle window also emits **zero** frames. Claude actively *working* still animates (the ✻
  bloom / avatar) as before. macOS behaviour is unchanged in feel; the same idle path applies.
- **Usage refresh button is now just an icon** (no live `M:SS` countdown). A per-second
  countdown requires a ~1Hz repaint, which is exactly the idle clock that was removed, so
  it can't coexist with true idle. The button still refetches on click, the auto-refresh
  still runs every 120s, and the per-meter reset times (e.g. "7d: 1h 41m") are unchanged.

## [1.0.10] — 2026-06-18

### Added
- **Workspace tab "running" dot:** a workspace tab now also shows a solid **green** dot when
  one of its terminals has a (non-Claude) command running — a build, dev server, `sleep`,
  vim, etc. Full priority across the workspace's terminals is now amber (needs attention) →
  blue (Claude working) → green (command running). The green dot is solid and appears/hides
  on command start/end (event-driven), so a long-running command adds no idle CPU.

### Fixed
- **Paste jumps to the prompt:** pasting (Cmd/Ctrl+V, middle-click, or a file attach via
  drag-drop / the pickers) while scrolled up in the scrollback now snaps the view back to
  the live bottom and clears the selection — matching what typing already did — so the
  pasted text is visible where it lands at the prompt.
- **Text selection while scrolling:** dragging a selection past the top/bottom edge now
  auto-scrolls continuously while the mouse is held still (instead of stalling after a
  moment), at a controllable speed, and scrolling the wheel while holding the button keeps
  extending the selection (instead of stopping after one notch). Root cause: the terminal's
  overlay layer (working bar / info popover / scroll indicator / find bar) was added or
  removed as the gesture progressed, which changed the widget tree and reset the terminal's
  per-widget interaction state mid-drag. The terminal is now always the base layer of a
  stable stack, so its drag/scroll state survives. The auto-scroll is also event-driven
  (a self-sustaining frame request), adding no idle CPU.

### Added
- **Terminal header right-click menu:** right-clicking a terminal's header now opens the same
  context menu as right-clicking its body (Rename, Clear Buffer, Split, Select All, Copy,
  Paste, Close), anchored at the cursor. The menu's actions — including Select All — target
  the terminal whose header was clicked.

### Fixed
- **Overview title alignment / wrapping:** a long terminal title no longer pushes the status
  dot and git stats out of their column or wraps to a second line. The title now truncates
  with an "…" (shorter still when git stats are present, since those take priority), and the
  row is clamped to a single line and clipped, so it can never grow to two lines regardless
  of title length. The status dot and git stats stay pinned to a fixed right column.
- **Usage "Sign in" flash on slow loads:** when the usage helper was slow to respond, the
  titlebar/overview briefly showed the "Sign in" button, then it vanished and the real usage
  appeared. It came from an 8s "still Loading → assume Sign in" fallback that a slow-but-
  successful load tripped. "Sign in" is now driven by what actually happened: the helper
  reporting `needs_login` (logged out) or the helper process exiting (not built / crashed,
  surfaced immediately), with the speculative timeout lengthened to 30s purely for a helper
  that's alive but silent — so a slow load just stays "Loading" until the data arrives.
- **Claude not relaunching on restart (intermittent, mainly Windows):** detecting that Claude
  is running in a pane is now driven by the statusLine capture Claude writes via our injected
  settings, rather than a process scan. The scan only looked for ~2s after a command started
  and then never re-checked (a running Claude keeps the shell busy, so no new "command started"
  edge), so a slow cold launch (shell profile + shim + node + MCP, antivirus) was missed —
  `claude_running` stayed false, wasn't persisted, and the next reopen didn't relaunch Claude.
  Now a capture appearing for a pane marks it running, independent of launch speed and fully
  event-driven; stale files can't false-trigger (capture dirs are cleared on startup and a
  pane's capture is deleted when its command ends). The process scan remains a fallback (now
  with backoff to 10s) for Claude started outside the shim.

## [1.0.9] — 2026-06-15

### Added
- **Per-terminal command history:** each terminal now keeps its own private command history
  that persists across app exit and relaunch — a reopened terminal recalls only the commands
  it ran, and a brand-new terminal starts empty. Each pane gets a stable id (saved in the
  layout) backing a private history file under `<data-dir>/history/`; the file is removed when
  the terminal/workspace is permanently closed and capped at the most recent 1000 commands.
  Works on macOS (zsh + bash) and Windows (PowerShell + Git Bash). Note: this is a deliberate
  departure from the usual shared-history model (iTerm2 et al. share one history file across tabs).

### Fixed
- **Overview row alignment:** the working ✻ animation no longer makes overview rows jump by a
  pixel when it shows/hides (its slot is now a fixed height, so a dot and the animation occupy
  the same space), and the git stats, status dot, and ✻ now sit level with the terminal titles.
- **Git footer across sibling terminals:** a git command (staging, commit, branch switch) in
  one terminal now refreshes the git footer of *other* terminals open in the same repo, not
  just the one that ran it. The repo watcher previously ignored all `.git/` changes to avoid a
  CPU loop (`git status` rewriting `.git/index`); status reads now use `--no-optional-locks`
  (read-only, no index write), so the watcher can safely observe meaningful `.git/` metadata
  while still ignoring object/log/lock churn. No extra idle CPU — the watcher stays purely
  event-driven.

## [1.0.8] — 2026-06-15

### Added
- **Quit confirmation:** closing Arbiter (the window close button, Cmd+Q on macOS, Alt+F4 on
  Windows) now asks for confirmation first, so a stray close can't silently drop every open
  terminal. Also fires on macOS logout / restart / shutdown, so a system quit can't silently
  kill running sessions. On by default; turn it off under Settings → General → Quitting.

### Changed
- **Escape closes dialogs:** pressing Escape now dismisses the open Settings, shortcuts,
  confirmation, and other modal dialogs / menus (falling through to the terminal only when
  none is open).
- **Borderless dialogs:** modal dialogs drop their hairline border for a cleaner, consistent
  look (matching the quit / close-workspace confirmations).

### Fixed
- **False Claude detection from dev servers:** running a node tool (e.g. `npm run dev`) no
  longer false-detects Claude in that pane (it showed an idle Claude dot). On macOS a process
  that rewrites its title (npm) makes `sysinfo` report its *environment* among its args, which
  surfaced Arbiter's own injected `ARBITER_*` vars — whose values point at `claude-shim` /
  `claude-sessions` / the real claude bin and so contained "claude". Detection now ignores
  `KEY=VALUE` env assignments and matches only the genuine claude-code CLI (its package dir or
  a `claude`-named bin).
- **Workspace tab alignment:** the activity dot is nudged down 1px so it sits level with the
  type icon + title (which don't move); the close (×) is sized down to 12px (its top-heavy
  glyph read high at 13px) and left centred.
- **Truecolor advertisement:** spawned shells now export `COLORTERM=truecolor`, so programs
  like Claude Code emit their real 24-bit palette (e.g. the vivid orange ✻) instead of a
  duller 256-colour approximation. Previously Arbiter relied on inheriting `COLORTERM`, which
  a Finder-launched app doesn't have — so colours looked vivid only when launched from a
  terminal that set it (iTerm2) and dull otherwise.
- **Nested Arbiter launches:** running `cargo run --bin arbiter` (or any Arbiter) from inside
  another Arbiter's terminal left `claude` hanging — the child inherited the parent's
  `claude`-shim dir at the front of `PATH` and resolved *it* as the "real" claude, so the
  shim `exec`'d itself in an infinite loop. The shim resolver now skips the parent shim
  (`ARBITER_SHIM_BIN`) and any directory whose `claude` is an Arbiter launcher, falling
  through to the genuine binary. Stats/statusLine/hooks are unchanged.

## [1.0.7] — 2026-06-14

### Added
- **Close-workspace confirmation:** closing a workspace — the tab ×, either type, or the
  right-click "Close" — now asks for confirmation first, so a stray click can't silently
  drop a workspace and its open terminals.
- **Workspace tab status dot:** a workspace tab now shows a pulsing dot (after its name,
  before the close button) when one of its terminals has Claude working (azure) or needing
  attention (amber). Attention takes priority across the workspace's terminals.

### Fixed
- **Launch focus:** the main window now reliably comes to the front and takes focus on
  launch, instead of sometimes opening unfocused behind other windows.
- **Windows glyphs:** fallback symbols like ✻ (Claude's working spinner) keep their full
  height instead of being squashed into the narrow cell — sized like Windows Terminal.
- **Windows Claude working-detection:** newline / mode edit keys (Shift+Enter, Ctrl+Enter,
  Shift+Tab), even pressed in rapid succession, or resizing the window, no longer falsely
  start Claude's "working" animation. Those keys make Windows ConPTY repaint and re-emit an
  on-screen ✻; they now briefly suppress spinner-detection so a burst can't pair into a
  false positive — while a plain Enter/submit clears the suppression, so genuine working is
  still detected immediately (no delay).

## [1.0.6] — 2026-06-14

### Changed
- **Much lower idle CPU.** The terminal now redraws on PTY output (event-driven) and
  the title is a static gradient, so the 60fps animation tick pauses (→ 1s) whenever
  nothing is animating. Idle goes from a constant ~1–2% repaint to near-zero; the fast
  tick returns only while Claude is working, a status needs attention, or the scroll
  indicator is fading.
- The git-status file watcher no longer pins the CPU in a repo: it ignores `.git/`
  churn (so `git status`'s own index rewrite can't re-trigger it in a loop) and
  gitignored build/dep dirs (`target/`, `node_modules/`, …).
- **Titlebar & overview polish:** a static azure title gradient (no pale blue); the
  overview titlebar matches the main window (centered on macOS, left-aligned on Windows,
  same indent); and the overview usage bars stay legible on the dark background.

### Added
- **Bold text style** setting (Settings → Display → Terminal), mirroring Windows
  Terminal's `intenseTextStyle`: render bold/intense (SGR 1) text as a **bold font**,
  a **brighter colour** (regular weight, the classic xterm look), **both**, or **none**.
- **Background colour** setting (Settings → Display → Terminal): presets (Default
  `#0a0a0c`, Gray `#121212`, Black) or a custom hex, applied live to the terminals,
  sidebars and the overview's terminal-list area. The terminal header tracks it.

### Fixed
- **Windows terminal text** is sharper and now matches Windows Terminal. Grayscale
  antialiasing uses DirectWrite's gamma-correct blend (the gamma-1.8 algorithm from
  WT's own shader) instead of a hazy/thin gamma-space blend; glyphs render in the
  recommended NATURAL_SYMMETRIC mode with grid-fitting; and **bold** renders from the
  bundled Cascadia Mono Bold face instead of a soft synthesised faux-bold.

## [1.0.5] — 2026-06-12

### Added
- Workspace tabs can be **dragged to reorder** (persistent), with a blue insertion
  line shown between tabs while dragging.
- **Overview window** redesign:
  - A Claude **usage-bar footer** that shares the main app's fetch (updates on the
    same timer + refresh), toggleable in Settings → Display → Overview. The bars hold
    the header size and only shrink (dropping reset times last) when the window is
    too narrow.
  - The **same custom titlebar as the main window** — centered logo + "Overview"
    with the azure glow behind it, Windows caption buttons / macOS traffic lights,
    resizable with a minimum size.
- Overview window **always-on-top** by default, with a Settings toggle.

### Changed
- macOS: the window can be dragged (logo / empty titlebar) *and* tabs reordered — the
  window drag is handled manually so the two no longer conflict.
- The usage refresh countdown no longer jumps backward when fresh data lands.

### Fixed
- **Windows Claude usage** updates reliably in the background again: stop WebView2
  throttling/occluding the hidden webview, drive it through a suspend/resume
  lifecycle, and recover a discarded renderer (after long idle) by reloading. The
  manual refresh button always recovers.
- **Windows glyph rendering**: blend coverage in gamma space (matches Windows
  Terminal) and fit oversized fallback glyphs (e.g. `⏵`, `✻`) into the cell instead
  of clipping or stretching.
- Dim/faint (SGR 2) terminal text now renders dimmer.

## [1.0.4] — 2026-06-12

### Added
- Right-click **context menus** for workspace tabs and terminals (rename, etc.).

## [1.0.3] — 2026-06-12

### Added
- Terminal query responses (DA / DSR / cursor-position) answered via a dedicated PTY
  writer thread, so apps that probe the terminal (e.g. biovpn) behave.

### Changed
- Rewrote the README for the native app; restored the Arbiter SVG logo.

## [1.0.2] — 2026-06-12

The **native rewrite** moves to the repo root: Arbiter is now a native Rust app
(iced + wgpu, no webview), replacing the Tauri/Vue web app.

### Added
- GPU terminal renderer (alacritty_terminal + wgpu) matching the web look, with
  CoreText (macOS) / DirectWrite (Windows) rasterization and color emoji.
- Event-driven Claude status (statusLine + hooks shim), per-pane footer stats, the
  working animation, and the popout Overview window.
- Claude usage bars via an isolated webview sidecar (claude.ai session), with org
  selection.
- Project workspaces: git worktrees, 3-pane layout, file explorer, worktree cards
  with avatars.
- Native unified titlebar (macOS traffic-light inset; Windows custom caption buttons
  + borderless resize), session persistence, keyboard shortcuts, file attach, and the
  Settings dialog.

### Fixed
- Windows: crisp caption glyphs; the claude shim re-prepends PATH so `claude` is
  intercepted; no console flash or stderr leak when forwarding the user's statusLine.

## [1.0.1] — 2026-06-11

### Changed
- Worktree + overview status indicators centered and stabilized.
- Debug builds use a separate data dir + Claude login from release.
- Titlebar spacing, helper dock-flash, DMG layout, and window-refocus-lag polish.

## [1.0.0] — 2026-06-11

- Initial native release.
