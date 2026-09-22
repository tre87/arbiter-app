# Ops Floor: the take

Source read to the `#[cfg(test)]` line; frames judged from `01-room.png`,
`02-states.png` and `03-poses.png`. `src/floor.rs` is untouched.

## 1. The direction

The room is lit like a diagram. One flat ambient covers wall, desk and floor, and the
lamps and fixtures are painted onto it as furniture. That is why a well-drawn room reads
as inert: nothing in it is lit by anything.

The take is built on one idea: **make light structural.** The room is dark, and
everything you see, you see because a particular lamp lit it. A ceiling fixture throws a
cone down the wall behind each occupied desk. The desk lamp pools on the desktop. The
screen throws its state colour onto the figure's face and onto the floor in front of the
desk. A free desk sits in the dark with its chair pushed in. Every object then casts one
shadow from one key light, which is what glues a figure to a wall and a desk to a floor.

Depth, silhouette and detail fall out of this without a new hue. The drama is value
contrast, so the azure and the amber stay the only saturated things in the picture, and
each is now also the brightest thing in its own pool. Ambient motion is the cheap route to
"alive" and rule 4 forbids it, correctly. This gets there standing still.

No rule blocks the take. Rule 2 is touched in one new place (step 3, the rim), in exactly
the way the existing spill already touches it: a desk paints its own colour at alpha
inside its own slot.

## 2. The build spec

Ordered. **Steps 1 to 3 carry the idea**; the build can stop after step 3 and have
landed it. Everything is static unless it says otherwise, so `Scene::animates` is
unchanged; the one animated quantity (the pool's breathing) already exists and stays
gated on `Desk::animates`.

All geometry is in `station()`'s row-relative scheme: `cx` is the slot centre, `r` the
row band top, and the `*_Y` constants are unchanged.

### Step 1. Palette: drop the ambient one step, add the light and shade constants

In the `palette` block.

| Constant | Was | Now | Why |
|---|---|---|---|
| `WALL` | `#19202` (0x19,0x20,0x29) | `#151b23` | The wall is now the dark thing the cones land on. |
| `WALL_SEAM` | `#131a22` | `#10161d` | Keeps its one-step drop under the new wall. |
| `FLOOR` | `#141b23` | `#11171e` | Same reason, so the floor bounce has somewhere to go. |
| `FLOOR_LINE` | `#1d2630` | `#1a222b` | Same. |

`GROUND`, `CEIL`, `TRIM` unchanged. `TRIM` now sits two steps over the wall instead of
one, which is what an edge needs to read at 3x.

New constants:

- `CEIL_LIGHT = rgb(0xb4, 0xc0, 0xcc)`. Cool white off the ceiling fixtures. Blue-grey
  like the fixtures themselves (`STEEL`) so a lit wall never drifts warm toward the
  amber. Used by the cones (step 2), the fixture undersides (step 4) and the shoulder
  highlight (step 3).
- `SHADE = Rgba(0, 0, 0, 0.24)`. Every cast shadow. One weight for all of them: a shadow
  darker under one object than under another reads as a second light source, and the
  room has one key light.
- `SHADOW_DX = -3`, `SHADOW_DY = 3`. The key light is the desk lamp, above and to the
  right of the figure, so everything casts down and to the left. Three is one grid step
  in this art. I have not seen these on a frame: if two vanishes into the outline or
  four detaches the shadow from its owner, move it, but keep it one number for the room.
- `CONE: [(i32, i32, i32, f32); 3]`, see step 2.

### Step 2. `light_and_shadow()`: the cone, the pools and the shadows (the proof, part 3)

New function, signature `fn light_and_shadow(b: &mut Buf, cx: i32, r: i32, d: Desk,
lean: i32, t: f32)`. Called from `station()` **in place of the "Light spill" block**,
which it absorbs, so it runs before the chair. `lean` is already computed above that
block.

Layer order inside, far to near:

1. **Ceiling cone**, occupied desks only. Three bands stepping down the wall from the
   fixture's width, each a shade dimmer, then a dithered skirt, then a brighter core:
   - `fill(cx-14, r+0,  28, 10, CEIL_LIGHT.alpha(0.11))`
   - `fill(cx-19, r+10, 38, 14, CEIL_LIGHT.alpha(0.08))`
   - `fill(cx-24, r+24, 48, 16, CEIL_LIGHT.alpha(0.05))`
   - `fill_checker(cx-29, r+40, 58, DESK_Y-40, CEIL_LIGHT.alpha(0.06))`
   - `fill(cx-6, r+0, 12, 30, CEIL_LIGHT.alpha(0.04))` (the core)

   It stops at `DESK_Y`; below that the desk lamp owns the light. 58 wide at the foot
   in a 76 slot leaves 18 dark pixels between neighbours, so cones never merge into a
   lit wall. The bands are widths 28, 38, 48, 58 because the fixture bar in the
   clerestory is 28 wide and each step adds ten, which reads as a beam rather than a
   ramp at this size.
2. **Screen pool**, the three existing fills, unchanged (the `b0` breathing stays
   as is; only `Working` and `Attention` vary with `t`).
3. **Floor bounce**, new: the same colour off the floor strip in front of the desk.
   `fy = r + ROW_H - 10` (the strip `room()` paints).
   - `fill_checker(cx-4, fy, 36, 10, g.alpha(b0))`
   - `fill(cx+4, fy, 22, 4, g.alpha(b0 * 0.7))`

   For rows above the last, this lands on that row's own strip; the walkway (step 7)
   stays dark. Rule 2: the desk paints its own state colour inside its own slot, as the
   spill does now.
4. **Wall shadows**, `SHADE`, each drawn as the whole silhouette offset by
   `(SHADOW_DX, SHADOW_DY)`. The object is drawn over it later and covers most of it;
   what survives is a 3px rim on the wall along the object's lower-left, and that rim is
   the depth cue:
   - Monitor: `fill(cx+3+SHADOW_DX, r+MON_TOP+SHADOW_DY, 26, MON_BOT-MON_TOP, SHADE)`.
   - Desktop underside: `fill(cx-16+SHADOW_DX, r+DESK_Y+5, 52, 3, SHADE)`. Not offset
     in y: it is the dark under a slab, and it fills the one-row gap between the
     desktop and the thigh that currently shows bare wall.
   - Figure (occupied): `fill(cx-25+lean+SHADOW_DX, r+HAIR_Y+SHADOW_DY, 10, HIP_Y-HAIR_Y, SHADE)`.
     Hair, head and torso as one blob; the head shows a 4px shadow on the wall above the
     chair back, which is the classic cue.
   - Chair back: `fill(ch+SHADOW_DX, r+40+lean+SHADOW_DY, 5, SEAT_Y-40, SHADE)` where
     `ch` is the chair x exactly as `station()` computes it (pushed in when empty).
5. **Contact shadow** under the chair on the floor line, matching the desk's existing
   one: `fill(ch-1, r+FLOOR_Y, 22, 2, Rgba(0, 0, 0, 0.3))`.

### Step 3. Rim light on the figure

In `station()`, inside `if occupied`, after the torso fills and before `match (d, job)`,
so the arms draw over it where they cross it:

```rust
if let Some(g) = glow {
    // The screen lights the face it is turned toward.
    b.fill(cx - 17 + lean, r + HEAD_Y + 4, 1, 6, g.alpha(0.45));
}
// Ceiling light on the shoulders, whatever the screen says.
b.fill(cx - 26 + lean, r + SHOULDER_Y, 13, 1, CEIL_LIGHT.alpha(0.25));
```

`cx-17` is the head's front column; `HEAD_Y+4` starts under the hair. This is a 1px
column and it does more than its size: an idle figure gets a grey rim from `SCREEN_OFF`,
a working one an azure rim, and that is the figure lit by its own screen. Not a pose
(rule 6): it is per state, identical across the five tasks. Do not put a rim on the
torso: the desktop and the arms cover its front column in every pose, I checked the
geometry, and it comes out as stray pixels.

### Step 4. Clerestory: the fixtures become lights

`clerestory()` takes `scene: &Scene` instead of `weather` (call site in `room()`;
weather is `scene.weather`).

- Sill shadow: after the sill `fill(0, 36, W, 2, TRIM)`, add `fill(0, 38, W, 2, SHADE)`.
  The sill now stands off the wall.
- Per column `c`, after the fixture bar: if `scene.desks[c] != Desk::Empty`, lit
  underside `fill(cx-13, 42, 26, 1, CEIL_LIGHT.alpha(0.55))` and replace the
  `STEEL.alpha(0.06)` spill with `fill(cx-16, 43, 32, 3, CEIL_LIGHT.alpha(0.10))`. An
  unlit column keeps the bar only. The row-0 cone (step 2) starts at `CEIL_H` at the
  same 28 width, so fixture and cone read as one light.
- The row-top strip in `room()` (`STEEL.alpha(0.05)`, 6px) stays: it is the ceiling of
  rows 1+ leaking, and those rows have no visible fixture.

### Step 5. Window depth (`window`)

Before the existing city loop, a far skyline of six blocks at half alpha so it takes on
the sky's own haze under every weather (under `Sunny` it comes out as a pale silhouette,
which is what distance looks like in daylight):

- `bw = 8 + hash(seed, i+51) % 8`, `bh = 8 + hash(seed, i+53) % 10`,
  `bx = x + i*w/6 + hash(seed, i+55) % 5`, colour `rgb(0x10, 0x16, 0x1e).alpha(0.45)`.
- One lit window per far block when `hash(seed, i+57) % 4 == 0`: `STEEL.alpha(0.35)`,
  1x1 at `(bx+3, y+h-bh+3)`.

The near blocks are unchanged and draw over it. Two layers is all parallax needs when
nothing moves.

### Step 6. Desk lamp: a pool worth the fixture (`station`, the "Desk light" block)

- Raise the pool: checker `LAMP_WARM.alpha(0.1)` to `0.16`, fill `0.07` to `0.12`.
  Checked: `LAMP_WARM` at 0.16 over `METAL_DK` lands at about `#3f4347`, a warm grey,
  nowhere near the amber.
- Reach the near end of the desk: `fill_checker(cx-14, r+DESK_Y, 20, 3, LAMP_WARM.alpha(0.08))`
  after the desktop is drawn, so the mug and the keyboard sit in light and the desk has
  a lit surface rather than a lit stripe.

### Step 7. Near floor: darker toward the viewer (`room`)

The walkway is nearest the viewer and farthest from every lamp, so it is the dark plane.

- Replace the `TRIM` line at `fy` with `FLOOR_LINE`, so the last row's floor strip and
  the walkway read as one floor with a joint in it, not a step.
- Add `fill(0, h-11, w, 6, Rgba(0, 0, 0, 0.10))` above the cable trunk. Two values of
  darkening (this and the trunk's own) is a gradient at this size.

Near dark, mid lit, far dark, windows dimmer still: four planes, no new geometry.

### Step 8. Wall clutter fixed by slot (`station`, new `fn wall_item`)

Lowest rank; drop first. `fn wall_item(b: &mut Buf, cx: i32, r: i32, variant: u32,
seed: i64)`, called from `station()` before `light_and_shadow()` (so the cone lights it),
only when occupied, `variant = hash(i, 0x5e) % 3`. It lives on the wall left of the
andon, above the chair back (`r+40`) and clear of the head (`r+30`):

- `0`: nothing. A third of the desks have a bare wall, which is what keeps the other
  two thirds from reading as wallpaper.
- `1`: a shelf. `fill(cx-37, r+16, 16, 1, DESK_LT)`, bracket `fill(cx-37, r+17, 1, 2, METAL_DK)`,
  three spines 2 wide at `cx-35 + k*3`, heights `5 + hash(seed, k) % 4` standing on
  `r+16`, colours `[POT, METAL_LT, PLANT_DK][k]`.
- `2`: a pinboard. `fill(cx-37, r+9, 15, 11, POT_DK)`, top edge `fill(cx-37, r+9, 15, 1, POT)`,
  two notes `fill(cx-35, r+11, 4, 5, PAPER.alpha(0.5))` and `fill(cx-29, r+12, 5, 4, PAPER.alpha(0.4))`.

No new colour. Nothing here is in `SHIRTS`, so a desk's clutter cannot be mistaken for
its occupant.

### What to look at when it is built

Run the sheet and open `01-room.png` and `02-states.png`:

- The three shadow rims (head, chair back, monitor) should be visible and the same
  darkness. If the head's is not there, the offset is too small.
- The `Attention` desk's amber pool must still be the loudest thing on the sheet, louder
  than any cone. If a cone competes, lower `CONE`'s alphas before touching anything
  amber.
- Cones must not join between neighbours. They should not, at 58 wide in 76.
- An empty desk in `02-states.png` should read as dark and free at a glance, before you
  find the pushed-in chair.

## 3. One implemented proof

`light_and_shadow()` and its constants, against the existing API. Drops in above
`station()`; the call replaces the "Light spill" block there (the `lean` binding is
already in scope at that point).

```rust
/// Cool white off the ceiling fixtures. Blue-grey like the fixtures themselves
/// (`STEEL`), so a lit wall never drifts warm and toward the amber.
const CEIL_LIGHT: Rgba = rgb(0xb4, 0xc0, 0xcc);

/// Every cast shadow. One weight for all of them: a shadow darker under one
/// object than under another reads as a second light source, and this room has
/// one key light, the desk lamp.
const SHADE: Rgba = Rgba(0, 0, 0, 0.24);

/// Where the key light throws things. The desk lamp hangs above and to the
/// right of the figure, so everything casts down and to the left. Three is one
/// grid step in this art: less vanishes into the outline, more detaches the
/// shadow from what cast it.
const SHADOW_DX: i32 = -3;
const SHADOW_DY: i32 = 3;

/// The ceiling cone as bands down the wall: half-width, top, height, alpha. It
/// starts at the fixture's 28 and widens ten per step, each step a shade
/// dimmer, and stops at the desktop, where the desk lamp takes over. Stepped
/// rather than ramped: banding is honest at this resolution.
const CONE: [(i32, i32, i32, f32); 3] = [
    (14, 0, 10, 0.11),
    (19, 10, 14, 0.08),
    (24, 24, 16, 0.05),
];

// Everything that lights or shades one workstation, painted onto bare wall and
// floor before any furniture goes down. Far to near: the cone, the screen's
// pool over it, the floor bounce, then the shadows, so a shadow darkens lit
// wall rather than being lit over. `lean` is the figure's upper-body lean, so
// the head's shadow follows the head.
fn light_and_shadow(b: &mut Buf, cx: i32, r: i32, d: Desk, lean: i32, t: f32) {
    let occupied = d != Desk::Empty;
    // The chair x, exactly as the chair itself is placed: pushed in when free.
    let ch = cx - 31 + if occupied { 0 } else { 13 };

    if occupied {
        for (half, top, h, a) in CONE {
            b.fill(cx - half, r + top, half * 2, h, CEIL_LIGHT.alpha(a));
        }
        b.fill_checker(cx - 29, r + 40, 58, DESK_Y - 40, CEIL_LIGHT.alpha(0.06));
        // A brighter core under the fixture is what makes three bands read as
        // one beam instead of three lighter wallpapers.
        b.fill(cx - 6, r, 12, 30, CEIL_LIGHT.alpha(0.04));
    }

    if let Some(g) = d.glow() {
        // Small on purpose: the glow says which state, the andon says "act
        // now". Only the two live states breathe, so a quiet room stays still.
        let b0 = match d {
            Desk::Working => 0.10 + 0.02 * (t * 1.6).sin(),
            Desk::Attention => 0.07 + 0.03 * (t * 1.9).sin(),
            _ => 0.04,
        };
        b.fill_checker(cx - 18, r + 14, 50, 44, g.alpha(b0 * 0.55));
        b.fill(cx - 8, r + 18, 38, 36, g.alpha(b0 * 0.7));
        b.fill(cx, r + 20, 28, 30, g.alpha(b0));

        // The same light off the floor strip in front of the desk, dithered and
        // a step dimmer. This is what turns the strip from a band into a floor.
        let fy = r + ROW_H - 10;
        b.fill_checker(cx - 4, fy, 36, 10, g.alpha(b0));
        b.fill(cx + 4, fy, 22, 4, g.alpha(b0 * 0.7));
    }

    // Each shadow is the object's whole silhouette, offset. The object is drawn
    // over it afterwards and covers most of it; what survives is a three-pixel
    // rim along its lower-left, and that rim is the entire depth cue.
    b.fill(
        cx + 3 + SHADOW_DX,
        r + MON_TOP + SHADOW_DY,
        26,
        MON_BOT - MON_TOP,
        SHADE,
    );
    // Under the desktop, not offset in y: the dark under a slab, which also
    // closes the one-row gap of bare wall between the desktop and the thigh.
    b.fill(cx - 16 + SHADOW_DX, r + DESK_Y + 5, 52, 3, SHADE);
    if occupied {
        b.fill(
            cx - 25 + lean + SHADOW_DX,
            r + HAIR_Y + SHADOW_DY,
            10,
            HIP_Y - HAIR_Y,
            SHADE,
        );
    }
    b.fill(ch + SHADOW_DX, r + 40 + lean + SHADOW_DY, 5, SEAT_Y - 40, SHADE);

    // Contact shadow under the chair, the weight the desk already uses, so the
    // two stand on the same floor.
    b.fill(ch - 1, r + FLOOR_Y, 22, 2, Rgba(0, 0, 0, 0.3));
}
```

Call site in `station()`, replacing the block that begins `// Light spill.`:

```rust
light_and_shadow(b, cx, r, d, lean, t);
```
