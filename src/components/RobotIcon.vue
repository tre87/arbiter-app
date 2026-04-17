<script setup lang="ts">
import { computed } from 'vue'
import { generateRobotIcon, generateRobotFrame, robotVersion } from '../utils/robotIcon'

const props = withDefaults(defineProps<{
  branchName: string
  size?: number
  animated?: boolean
}>(), { size: 32, animated: false })

const staticSrc = computed(() => {
  void robotVersion.value
  return generateRobotIcon(props.branchName, props.size)
})

const frames = computed(() => {
  void robotVersion.value
  return [0, 1, 2, 3].map(i => generateRobotFrame(props.branchName, props.size, i))
})
</script>

<template>
  <img
    v-if="!animated"
    :src="staticSrc"
    :width="size"
    :height="size"
    class="robot-icon"
    :alt="branchName"
  />
  <span
    v-else
    class="robot-icon robot-anim"
    :style="{ width: size + 'px', height: size + 'px' }"
  >
    <img
      v-for="(src, i) in frames"
      :key="i"
      :src="src"
      :width="size"
      :height="size"
      class="robot-frame"
      :style="{ animationDelay: (i * 0.3) + 's' }"
      :alt="branchName"
    />
  </span>
</template>

<style scoped>
.robot-icon {
  border-radius: 4px;
  flex-shrink: 0;
}
.robot-anim {
  display: inline-block;
  position: relative;
  vertical-align: middle;
}
.robot-frame {
  position: absolute;
  top: 0;
  left: 0;
  opacity: 0;
  border-radius: 4px;
  animation: robot-blink 1.2s steps(1, end) infinite;
}
@keyframes robot-blink {
  0%, 25% { opacity: 1; }
  25.01%, 100% { opacity: 0; }
}
</style>
