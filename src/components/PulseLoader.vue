<template>
  <span class="pulse-loader" :style="{ gap: gap, '--dot-size': size }">
    <span class="dot" />
    <span class="dot" />
    <span class="dot" />
  </span>
</template>

<script setup lang="ts">
withDefaults(defineProps<{ size?: string; gap?: string }>(), {
  size: '4px',
  gap: '4px',
})
</script>

<style scoped>
.pulse-loader {
  display: inline-flex;
  align-items: center;
}

.dot {
  width: var(--dot-size);
  height: var(--dot-size);
  border-radius: 50%;
  background: var(--color-accent);
  opacity: 0.3;
  animation: pulse-dot 1.2s ease-in-out infinite;
}

.dot:nth-child(2) { animation-delay: 0.2s; }
.dot:nth-child(3) { animation-delay: 0.4s; }

@keyframes pulse-dot {
  0%, 80%, 100% { opacity: 0.2; transform: scale(0.8); }
  40%           { opacity: 1;   transform: scale(1); }
}
</style>
