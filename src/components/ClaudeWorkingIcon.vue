<script setup lang="ts">
import { ref, computed, onMounted, onBeforeUnmount } from 'vue'

// Mirrors Claude Code's CLI "thinking" glyph: it cycles the asterisk/star
// family · ✢ ✳ ✶ ✻ ✽ (a dot blooming open into a full star and collapsing
// back), tinting each step a brighter shade of Claude orange. Claude eases the
// cycle so the first/last glyph linger a touch longer — approximated here by
// holding the endpoints for one extra frame.
// Refs: blog.alexbeals.com/posts/claude-codes-thinking-animation and Kyle
// Martinez's "Reverse Engineering Claude's ASCII Spinner Animation".

const props = withDefaults(defineProps<{ size?: number }>(), { size: 12 })

// Ping-pong over the glyph family with the endpoints doubled for the "ease".
const FRAMES = ['·', '·', '✢', '✳', '✶', '✻', '✽', '✽', '✻', '✶', '✳', '✢'] as const

// Dim → bright shades of Claude orange, tracking how "open" the star is.
const COLOR: Record<string, string> = {
  '·': '#9c5638',
  '✢': '#b86a45',
  '✳': '#c97a52',
  '✶': '#d9885f',
  '✻': '#e89870',
  '✽': '#f4ad88',
}

const i = ref(0)
const glyph = computed(() => FRAMES[i.value])
const color = computed(() => COLOR[FRAMES[i.value]])

let timer: ReturnType<typeof setInterval> | undefined
onMounted(() => {
  timer = setInterval(() => { i.value = (i.value + 1) % FRAMES.length }, 110)
})
onBeforeUnmount(() => { if (timer) clearInterval(timer) })
</script>

<template>
  <span
    class="claude-working"
    :style="{ fontSize: size + 'px', lineHeight: 1, color }"
    aria-label="Claude working"
  >{{ glyph }}</span>
</template>

<style scoped>
.claude-working {
  display: inline-block;
  font-weight: 700;
  /* Keep the cell width steady as the glyph changes width so neighbours don't
     shift. */
  width: 1em;
  text-align: center;
  transition: color 0.11s linear;
}
</style>
