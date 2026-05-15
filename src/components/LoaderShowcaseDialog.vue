<script setup lang="ts">
import { onMounted, onBeforeUnmount } from 'vue'
import NeuralPulse from './NeuralPulse.vue'
import CometTrail from './CometTrail.vue'
import HexPulse from './HexPulse.vue'
import ParticleOrbV2 from './ParticleOrbV2.vue'
import PulseLoader from './PulseLoader.vue'

const emit = defineEmits<{ (e: 'close'): void }>()

function onKey(e: KeyboardEvent) {
  if (e.key === 'Escape') { e.preventDefault(); emit('close') }
}

onMounted(() => window.addEventListener('keydown', onKey, { capture: true }))
onBeforeUnmount(() => window.removeEventListener('keydown', onKey, { capture: true }))

const sizes = [12, 14, 18, 24, 40]
const orbV2Sizes = [12, 24, 40, 64, 96]
</script>

<template>
  <Teleport to="body">
    <div class="dialog-overlay" @mousedown.self="emit('close')">
      <div class="dialog showcase-dialog">
        <div class="dialog-header">
          <h3 class="dialog-title">Loader Showcase</h3>
          <button class="close-btn" @click="emit('close')">×</button>
        </div>

        <p class="dialog-hint">Hover to isolate.</p>

        <div class="grid">
          <div class="row">
            <div class="row-label">
              <span class="label-name">Neural Pulse</span>
              <span class="label-desc">3 nodes, pulses travel along edges in sequence</span>
            </div>
            <div class="sizes">
              <div v-for="s in sizes" :key="s" class="cell">
                <NeuralPulse :size="s" />
                <span class="cell-size">{{ s }}px</span>
              </div>
            </div>
          </div>

          <div class="row">
            <div class="row-label">
              <span class="label-name">Comet Trail</span>
              <span class="label-desc">bright head with a fading arc tail, rotates around center</span>
            </div>
            <div class="sizes">
              <div v-for="s in sizes" :key="s" class="cell">
                <CometTrail :size="s" />
                <span class="cell-size">{{ s }}px</span>
              </div>
            </div>
          </div>

          <div class="row">
            <div class="row-label">
              <span class="label-name">Hex Pulse</span>
              <span class="label-desc">hexagon outline, each side brightens in sequence around the ring</span>
            </div>
            <div class="sizes">
              <div v-for="s in sizes" :key="s" class="cell">
                <HexPulse :size="s" />
                <span class="cell-size">{{ s }}px</span>
              </div>
            </div>
          </div>

          <div class="row existing">
            <div class="row-label">
              <span class="label-name">Pulse Loader <span class="tag">existing</span></span>
              <span class="label-desc">three dots — current overview loader</span>
            </div>
            <div class="sizes">
              <div v-for="s in sizes" :key="s" class="cell">
                <PulseLoader :size="Math.max(2, Math.round(s / 4)) + 'px'" :gap="Math.max(2, Math.round(s / 5)) + 'px'" />
                <span class="cell-size">{{ s }}px</span>
              </div>
            </div>
          </div>
        </div>

        <div class="section-label">Large-format (needs room)</div>
        <div class="grid">
          <div class="row">
            <div class="row-label">
              <span class="label-name">Particle Orb v2</span>
              <span class="label-desc">3D: particles twinkle in place on a rotating sphere — no explosion, works at smaller sizes</span>
            </div>
            <div class="sizes">
              <div v-for="s in orbV2Sizes" :key="s" class="cell">
                <ParticleOrbV2 :size="s" />
                <span class="cell-size">{{ s }}px</span>
              </div>
            </div>
          </div>
        </div>

        <div class="context-row">
          <div class="context-card">
            <div class="context-header">Worktree card context (≈12px beside status label)</div>
            <div class="context-examples">
              <div class="ctx-ex"><NeuralPulse :size="12" /><span class="status-text">Neural · working</span></div>
              <div class="ctx-ex"><CometTrail :size="12" /><span class="status-text">Comet · working</span></div>
              <div class="ctx-ex"><HexPulse :size="12" /><span class="status-text">Hex · working</span></div>
            </div>
          </div>
          <div class="context-card dark">
            <div class="context-header">Overview row context (≈10px dot slot)</div>
            <div class="context-examples">
              <div class="ctx-ex overview"><NeuralPulse :size="10" /><span class="ov-name">Neural pane</span></div>
              <div class="ctx-ex overview"><CometTrail :size="10" /><span class="ov-name">Comet pane</span></div>
              <div class="ctx-ex overview"><HexPulse :size="10" /><span class="ov-name">Hex pane</span></div>
            </div>
          </div>
        </div>
      </div>
    </div>
  </Teleport>
</template>

<style scoped>
.dialog-overlay {
  position: fixed;
  inset: 0;
  background: rgba(0, 0, 0, 0.6);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 9999;
}

.dialog {
  background: var(--color-bg-elevated);
  border: 1px solid var(--color-card-border);
  border-radius: 8px;
  padding: 20px 24px;
  min-width: 560px;
  max-width: 680px;
  max-height: 92vh;
  overflow-y: auto;
  box-shadow: 0 8px 32px rgba(0, 0, 0, 0.5);
}

.section-label {
  font-size: 10px;
  font-weight: 600;
  color: var(--color-text-muted);
  text-transform: uppercase;
  letter-spacing: 0.08em;
  margin: 14px 0 6px;
}

.dialog-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  margin-bottom: 6px;
}

.dialog-title {
  margin: 0;
  font-size: 14px;
  font-weight: 600;
  color: var(--color-text-primary);
}

.close-btn {
  background: none;
  border: none;
  color: var(--color-text-muted);
  font-size: 18px;
  line-height: 1;
  cursor: pointer;
  padding: 0 6px;
  border-radius: 3px;
}

.close-btn:hover {
  color: var(--color-text-primary);
  background: var(--color-card-border);
}

.dialog-hint {
  margin: 0 0 14px;
  font-size: 11px;
  color: var(--color-text-muted);
}

.grid {
  display: flex;
  flex-direction: column;
  gap: 2px;
  border: 1px solid var(--color-card-border);
  border-radius: 6px;
  overflow: hidden;
}

.row {
  display: grid;
  grid-template-columns: 180px 1fr;
  gap: 12px;
  padding: 14px 16px;
  background: var(--color-bg-subtle);
  align-items: center;
  transition: background 0.15s;
}

.row:hover {
  background: var(--color-bg);
}

.row + .row {
  border-top: 1px solid var(--color-card-border);
}

.row.existing {
  opacity: 0.75;
}

.row-label {
  display: flex;
  flex-direction: column;
  gap: 3px;
}

.label-name {
  font-size: 12px;
  font-weight: 600;
  color: var(--color-text-primary);
}

.label-desc {
  font-size: 10px;
  color: var(--color-text-muted);
}

.tag {
  display: inline-block;
  margin-left: 6px;
  padding: 1px 6px;
  background: var(--color-card-border);
  color: var(--color-text-muted);
  border-radius: 3px;
  font-size: 9px;
  font-weight: 500;
  letter-spacing: 0.05em;
  text-transform: uppercase;
  vertical-align: middle;
}

.sizes {
  display: flex;
  gap: 20px;
  align-items: center;
}

.cell {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 6px;
  min-width: 44px;
}

.cell-size {
  font-size: 10px;
  color: var(--color-text-muted);
  font-variant-numeric: tabular-nums;
}

.context-row {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 10px;
  margin-top: 14px;
}

.context-card {
  background: var(--color-bg-subtle);
  border: 1px solid var(--color-card-border);
  border-radius: 6px;
  padding: 10px 12px;
}

.context-card.dark {
  background: var(--color-bg);
}

.context-header {
  font-size: 10px;
  font-weight: 600;
  color: var(--color-text-muted);
  text-transform: uppercase;
  letter-spacing: 0.06em;
  margin-bottom: 8px;
}

.context-examples {
  display: flex;
  flex-direction: column;
  gap: 6px;
}

.ctx-ex {
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 11px;
}

.status-text {
  color: var(--azure);
  font-weight: 600;
}

.ctx-ex.overview {
  justify-content: space-between;
  padding: 4px 2px;
  flex-direction: row-reverse;
}

.ov-name {
  color: var(--color-text-primary);
  font-size: 11px;
}
</style>
