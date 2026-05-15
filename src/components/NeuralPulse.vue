<script setup lang="ts">
withDefaults(defineProps<{ size?: number }>(), { size: 16 })
</script>

<template>
  <svg
    :width="size"
    :height="size"
    viewBox="0 0 24 24"
    class="neural"
  >
    <line class="edge e1" x1="5" y1="19" x2="19" y2="19" />
    <line class="edge e2" x1="19" y1="19" x2="12" y2="5" />
    <line class="edge e3" x1="12" y1="5" x2="5" y2="19" />
    <circle class="node n1" cx="5" cy="19" r="2.3" />
    <circle class="node n2" cx="19" cy="19" r="2.3" />
    <circle class="node n3" cx="12" cy="5" r="2.3" />
  </svg>
</template>

<style scoped>
.neural {
  display: inline-block;
  vertical-align: middle;
  overflow: visible;
}

.edge {
  stroke: var(--azure);
  stroke-width: 1.2;
  stroke-linecap: round;
  fill: none;
  opacity: 0.35;
  stroke-dasharray: 5 30;
  animation: neural-pulse 1.8s linear infinite;
}

.e2 { animation-delay: 0.6s; }
.e3 { animation-delay: 1.2s; }

.node {
  fill: var(--azure-deep);
  filter: drop-shadow(0 0 1.5px var(--azure));
  transform-box: fill-box;
  transform-origin: center;
  animation: neural-node 1.8s ease-out infinite;
}

.n1 { animation-delay: 0s; }
.n2 { animation-delay: 0.6s; }
.n3 { animation-delay: 1.2s; }

@keyframes neural-pulse {
  0%   { stroke-dashoffset: 35; opacity: 0.15; }
  30%  { opacity: 1; }
  100% { stroke-dashoffset: 0;  opacity: 0.2; }
}

@keyframes neural-node {
  0%, 100% { transform: scale(1);   fill: var(--azure-deep); }
  15%      { transform: scale(1.35); fill: var(--azure-baby); }
  40%      { transform: scale(1);   fill: var(--azure-deep); }
}
</style>
