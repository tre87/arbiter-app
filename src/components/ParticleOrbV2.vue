<script setup lang="ts">
import { onMounted, onBeforeUnmount, ref, watch } from 'vue'

const props = withDefaults(defineProps<{
  size?: number
  quantity?: number
  particleCycle?: number
  rotateCycle?: number
}>(), {
  size: 64,
  quantity: 0,
  particleCycle: 7000,
  rotateCycle: 20000,
})

const wrapRef = ref<HTMLDivElement | null>(null)
let animations: Animation[] = []
let rotateAnim: Animation | null = null
let particleEls: HTMLDivElement[] = []

function clamp(n: number, lo: number, hi: number) {
  return Math.max(lo, Math.min(hi, n))
}

function start() {
  const wrap = wrapRef.value
  if (!wrap || typeof wrap.animate !== 'function') return

  const size = props.size
  const total = props.quantity > 0
    ? props.quantity
    : clamp(Math.round(size * 3), 80, 600)
  const orbRadius = size * 0.42
  const particleSize = size < 80 ? 1.5 : clamp(Math.round(size / 50), 2, 3)
  const duration = props.particleCycle

  for (let i = 0; i < total; i++) {
    const c = document.createElement('div')
    c.className = 'c'
    c.style.width = `${particleSize}px`
    c.style.height = `${particleSize}px`

    const t = i / total
    const hue = 199 + t * 12
    const sat = 78 + Math.random() * 22
    const light = 55 + Math.random() * 25
    c.style.backgroundColor = `hsla(${hue}, ${sat}%, ${light}%, 1)`

    wrap.appendChild(c)
    particleEls.push(c)

    const z = Math.random() * 360
    const y = Math.random() * 360
    const form = `rotateZ(${-z}deg) rotateY(${y}deg) translateX(${orbRadius}px) rotateZ(${z}deg)`

    const anim = c.animate([
      { opacity: 0, transform: 'none', offset: 0 },
      { opacity: 1, offset: 0.2 },
      { opacity: 1, transform: form, offset: 0.3 },
      { opacity: 1, transform: form, offset: 0.8 },
      { opacity: 0, transform: form, offset: 1 },
    ], {
      duration,
      delay: -(t * duration),
      iterations: Infinity,
      easing: 'linear',
    })
    animations.push(anim)
  }

  rotateAnim = wrap.animate([
    { transform: 'rotateY(0deg) rotateX(0deg)' },
    { transform: 'rotateY(360deg) rotateX(360deg)' },
  ], {
    duration: props.rotateCycle,
    iterations: Infinity,
    easing: 'linear',
  })
}

function stop() {
  for (const a of animations) {
    try { a.cancel() } catch { /* ignore */ }
  }
  animations = []
  if (rotateAnim) {
    try { rotateAnim.cancel() } catch { /* ignore */ }
    rotateAnim = null
  }
  for (const el of particleEls) el.remove()
  particleEls = []
}

onMounted(start)
onBeforeUnmount(stop)
watch(
  [() => props.size, () => props.quantity, () => props.particleCycle, () => props.rotateCycle],
  () => { stop(); start() }
)
</script>

<template>
  <div class="particle-orb-v2" :style="{ width: `${size}px`, height: `${size}px` }">
    <div ref="wrapRef" class="wrap" />
  </div>
</template>

<style scoped>
.particle-orb-v2 {
  display: inline-block;
  vertical-align: middle;
  position: relative;
  overflow: visible;
}

.wrap {
  position: absolute;
  top: 50%;
  left: 50%;
  width: 0;
  height: 0;
  transform-style: preserve-3d;
  perspective: 600px;
}

.wrap :deep(.c) {
  position: absolute;
  top: 0;
  left: 0;
  border-radius: 50%;
  opacity: 0;
  will-change: transform, opacity;
}
</style>
