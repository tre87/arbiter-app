<script setup lang="ts">
withDefaults(defineProps<{ size?: number }>(), { size: 16 })
</script>

<template>
  <svg :width="size" :height="size" viewBox="0 0 24 24" class="hex-pulse">
    <line class="side s1" x1="12" y1="3.5" x2="19.36" y2="7.75" vector-effect="non-scaling-stroke" />
    <line class="side s2" x1="19.36" y1="7.75" x2="19.36" y2="16.25" vector-effect="non-scaling-stroke" />
    <line class="side s3" x1="19.36" y1="16.25" x2="12" y2="20.5" vector-effect="non-scaling-stroke" />
    <line class="side s4" x1="12" y1="20.5" x2="4.64" y2="16.25" vector-effect="non-scaling-stroke" />
    <line class="side s5" x1="4.64" y1="16.25" x2="4.64" y2="7.75" vector-effect="non-scaling-stroke" />
    <line class="side s6" x1="4.64" y1="7.75" x2="12" y2="3.5" vector-effect="non-scaling-stroke" />
  </svg>
</template>

<style scoped>
.hex-pulse {
  display: inline-block;
  vertical-align: middle;
  overflow: visible;
}

.side {
  stroke: var(--azure);
  stroke-width: 1.6;
  stroke-linecap: round;
  fill: none;
  opacity: 0.3;
  animation: hex-side 1.8s ease-in-out infinite;
}

.s1 { animation-delay: 0s; }
.s2 { animation-delay: 0.25s; }
.s3 { animation-delay: 0.5s; }
.s4 { animation-delay: 0.75s; }
.s5 { animation-delay: 1s; }
.s6 { animation-delay: 1.25s; }

@keyframes hex-side {
  0%, 100% { opacity: 0.25; stroke: var(--azure); }
  15%      { opacity: 1;    stroke: var(--azure-baby); }
}
</style>
