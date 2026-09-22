# Ops Floor: a livelier take

You are being asked for a **design**, not a finished implementation. Another model
(Opus) will build whatever you specify, in this repo, right after you. Your budget is
small and fixed, so spend it on judgement and specifics, not on typing out the whole
room.

Work in one pass. Do not ask clarifying questions, do not explore the repo beyond what
is listed below, and do not edit `src/floor.rs`. Write one file and stop.

## What this is

Arbiter runs many Claude Code sessions side by side. The Ops Floor is a pixel-art room
where **one desk is one pane**: an ambient display somebody leaves open in the corner of
their eye to see, at a glance, which agents are working, which are blocked, and which
have finished. It is drawn in code, not imported. Every pixel is a rectangle fill, there
are no assets and no dependencies, and `render(&Scene, t) -> Buf` is a pure function to
an RGBA buffer.

The whole room lives in `src/floor.rs`. **Read it, stopping at the `#[cfg(test)]` line
(1117).** The tests below that line are summarised in "Rules" and you do not need to read
them.

## Look at it first

```
cargo test --lib floor::tests::sheet -- --ignored --nocapture
```

That writes PNGs to `%TEMP%\ops-floor\` (3x nearest-neighbour, so they are legible) and
prints each path. **Open `01-room.png` and `03-poses.png` at minimum.** `02-states.png`
has every state side by side and `04-sky-*.png` covers the weather; look at those only if
you have budget spare. You cannot judge this from the source, and every correction the
room has already had came from an eye on a frame rather than from reading the code.

## The brief

The owner likes the current art. This is not a rescue. The ask, in their words, is a
**"more live and exciting take"**.

My own read of `01-room.png`, to argue with rather than to accept: the room is a single
flat plane. Nothing overlaps anything, every desk sits at the same depth, the wall between
the desk lights and the clerestory is a large empty rectangle, and the near floor is a
thin dark band with nothing happening on it. It is legible and calm, and it is also
somewhat inert. Depth, lighting and detail density all look like they have room, and none
of them costs a reserved colour.

### The tension you have to resolve

The room is deliberately low saturation and biased cool so that the two semantic colours
are the only things the eye is pulled toward. "Exciting" pushes directly against that.

Resolve it in favour of the semantics. Find the life in **composition, depth, lighting,
silhouette and detail density**, not in turning up the saturation of the furniture. If the
room gets louder, the AZURE and the AMBER stop meaning anything, and then the display has
no purpose. If you think a rule below genuinely blocks a better room, say so explicitly in
your output and make the argument. Do not quietly break one.

## Rules (enforced by tests, not by taste)

1. **A desk never moves.** Slot `i` is always at row `i / 5`, column `i % 5`, for the life
   of the pane. Its clutter is fixed by slot too. You learn where things are; that is the
   room's only advantage over a list.
2. **Colour is a reserved word.** `WORKING` (`#3399ff`) is painted by nothing except a desk
   in `Desk::Working`, `ATTENTION` (`#e5a03c`) by nothing except `Desk::Attention`, `DONE`
   (`#22c55e`) by nothing except `Desk::Done`. Furniture, weather and work poses may not
   borrow any of them, under any sky, in any pose. A test checks every combination.
3. **Nothing is random.** Variety comes from `hash(a, b)` / `hashf(a, b)`, never from stored
   state or an RNG, so `render` stays a pure function of `(scene, t)` and any frame
   reproduces.
4. **Only real activity animates.** `Scene::animates()` must stay false for a room of quiet
   desks under a still sky: no clock, no frames, nothing. Rain and snow are the one allowed
   exception and `Weather::moves` makes that explicit. Anything ambient that moves on its
   own breaks this, and this app has a hard no-polling rule behind it.
5. **No new dependencies, no asset files, no text in the art.** Rectangle fills only, via
   `Buf::fill` and `Buf::fill_checker`.
6. **Every pose means the same thing.** The five work poses are cosmetic variety so that
   five agents mid-turn are not five copies of one sprite. None of them is a mood, and none
   may introduce a colour or a motion the other states lack.
7. **Pacing is slow on purpose.** `SPIN_HZ` 3.0, `OUT_HZ` 1.1, `SPRITE_FPS` 7.0. This is a
   picture in the corner of somebody's eye, not a progress bar. Motion that pulls the eye
   off their actual work is a defect.

## What has already been tried and rejected

Four rounds of correction are in the branch history. Do not spend budget rediscovering
them:

- **Figures that read as standing behind their desks.** Fixed by a seated pose: hip on the
  seat, thigh forward under a cantilevered desktop whose pedestal is on the far side, shin
  and foot down to the floor.
- **A raised paddle to signal "blocked".** It appeared from nowhere, so the eye was caught
  by a new object rather than by a colour, and two dark pixels on amber read as eyes, which
  made a mascot. Replaced by an andon, a lamp bolted over every desk, always the same shape,
  dark until needed.
- **Plants hanging from the clerestory sill.** Made the ceiling read as a shelf. All
  greenery is on desks now.
- **Plants standing on the near floor.** Read as a garden centre.
- **A deep band of empty floor tiles under the last row.** Read as a missing wall. The near
  floor is a shallow walkway now.
- **Terminal-speed animation.** The spinner turned twice a second and the screen glow
  breathed at 1.4Hz, which is a flicker rather than a glow.

Also settled: the room grows **downward**, five desks to a row, and is blitted only at
whole-number scales. Sorting desks by urgency is rejected forever (rule 1). Scrolling the
room is rejected (it puts desks off-screen, which is the whole point lost).

## What to produce

Write **one file, `FABLE-TAKE.md`, in the repo root.** Nothing else. Three parts:

### 1. The direction (roughly 150 words)

What makes this room feel alive, stated as an argument someone can disagree with. Name the
one idea the take is built on. Not a mood board and not adjectives.

### 2. The build spec

An ordered list of concrete changes, written for someone who will implement them without
you. This is the part that matters, so give it most of your budget. Specifics only:

- Hex values for any colour you add, and where each is used.
- Pixel geometry: coordinates, sizes and layer order, in the same row-relative scheme
  `station()` already uses (`cx`, `r`, and the `*_Y` constants).
- Any new constants, with the reason for the number.
- Which existing function each change lands in (`room`, `clerestory`, `window`, `screen`,
  `station`, `desk_plant`), or the signature of a new one.
- For anything that moves: what drives it, and which of `Desk::animates` or `Weather::moves`
  has to agree.
- Rank the list. Say which three changes carry most of the effect, so the build can stop
  early and still land the idea if it has to.

### 3. One implemented proof

Pick the single piece that best carries the direction and write it as real Rust against the
existing API (`Buf::fill`, `Buf::fill_checker`, `Rgba`, `rgb`, `alpha`, `hash`, `hashf`, and
the layout constants). A whole function, in a code block, ready to be dropped in. It does
not have to compile first time; getting it to compile is Opus's job. It does have to show
the taste, because that is the thing prose cannot carry.

Prefer the piece with the highest ratio of effect to code. If the direction lives in the
room rather than in the furniture, that is `room()`; if it lives in the workstation, that is
part of `station()`.

## House style for anything you write

No em dashes or en dashes in prose or comments; use a comma, a colon, parentheses or a
separate sentence. Comments explain why, never what: a magic number earns one, a statement
narrating the next line does not.
