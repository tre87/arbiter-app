// DiceBear bottts-based robot icon generator
// Deterministic: same branch name always produces the same robot (until regenerated)

import { createAvatar } from '@dicebear/core'
import { bottts } from '@dicebear/collection'
import type { Options as BotttsOptions } from '@dicebear/bottts'

// Animation eye cycle — keeps the robot's identity intact, only swaps eyes per frame
// Frame 0,1: default (seeded) eyes — open and neutral
// Frame 2: 'happy'  — looks like closed/blinking eyes
// Frame 3: 'glow'   — bright "looking" variant
type BotttsEye =
  | 'bulging' | 'dizzy' | 'eva' | 'frame1' | 'frame2' | 'glow' | 'happy'
  | 'hearts' | 'robocop' | 'round' | 'roundFrame01' | 'roundFrame02'
  | 'sensor' | 'shade01'

// Per-frame eye override (undefined = let seed pick)
const ANIM_EYES: Array<BotttsEye | undefined> = [
  undefined,   // open (seeded default)
  undefined,   // open (seeded default)
  'happy',     // blink / closed-ish
  'glow',      // "looking" variant
]

const cache = new Map<string, string>()
const seedOffsets = new Map<string, number>()

export function regenerateRobot(branchName: string) {
  const current = seedOffsets.get(branchName) ?? 0
  seedOffsets.set(branchName, current + 1)
  for (const key of Array.from(cache.keys())) {
    if (key.startsWith(branchName + ':')) cache.delete(key)
  }
}

function getSeed(branchName: string): string {
  const offset = seedOffsets.get(branchName) ?? 0
  return offset === 0 ? branchName : `${branchName}__seed${offset}`
}

function render(branchName: string, size: number, eyeOverride?: BotttsEye): string {
  const seed = getSeed(branchName)
  const options: BotttsOptions & { seed: string; size: number } = {
    seed,
    size,
  }
  if (eyeOverride) {
    options.eyes = [eyeOverride]
  }
  return createAvatar(bottts, options).toDataUri()
}

export function generateRobotIcon(branchName: string, size: number = 32): string {
  const key = `${branchName}:${size}:static`
  const cached = cache.get(key)
  if (cached) return cached
  const dataUrl = render(branchName, size)
  cache.set(key, dataUrl)
  return dataUrl
}

export function generateRobotFrame(branchName: string, size: number, frame: number): string {
  const f = ((frame % ANIM_EYES.length) + ANIM_EYES.length) % ANIM_EYES.length
  const key = `${branchName}:${size}:f${f}`
  const cached = cache.get(key)
  if (cached) return cached
  const dataUrl = render(branchName, size, ANIM_EYES[f])
  cache.set(key, dataUrl)
  return dataUrl
}
