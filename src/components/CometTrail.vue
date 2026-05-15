<script setup lang="ts">
withDefaults(defineProps<{ size?: number }>(), { size: 16 })
</script>

<template>
  <svg :width="size" :height="size" viewBox="0 0 24 24" class="comet">
    <g class="rotor">
      <path class="tail" d="M 12 3 A 9 9 0 0 0 3 12" vector-effect="non-scaling-stroke" />
      <circle class="head" cx="12" cy="3" r="2.1" />
    </g>
  </svg>
</template>

<style scoped>
.comet {
  display: inline-block;
  vertical-align: middle;
  overflow: visible;
}

.rotor {
  transform-origin: 12px 12px;
  animation: comet-spin 1.1s linear infinite;
}

.tail {
  fill: none;
  stroke: var(--azure);
  stroke-width: 2;
  stroke-linecap: round;
  opacity: 0.45;
}

.head {
  fill: var(--azure-baby);
  filter: drop-shadow(0 0 1.5px var(--azure));
}

@keyframes comet-spin {
  to { transform: rotate(360deg); }
}
</style>
