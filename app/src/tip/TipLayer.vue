<script setup lang="ts">
import { useTemplateRef, watch } from 'vue'
import { tip } from '@/tip/tip'

// 全局唯一的悬浮提示。放在顶层（popover），对话框打开时也在最上面。
/** 与锚点的间距（Fluent tooltip 的 spacingVerticalXS） */
const GAP = 4
/** 与窗口边缘的最小距离 */
const EDGE = 8

const layer = useTemplateRef<HTMLDivElement>('layer')

/** 放在锚点下方、左边对齐；下方放不下翻到上方，右边放不下向左移 */
function place(el: HTMLElement, anchor: HTMLElement): void {
  // 先放到左上角再量尺寸，免得上一次靠右的位置把宽度挤窄
  el.style.left = '0px'
  el.style.top = '0px'
  const box = anchor.getBoundingClientRect()
  const { width, height } = el.getBoundingClientRect()
  const left = Math.max(EDGE, Math.min(box.left, window.innerWidth - EDGE - width))
  const below = box.bottom + GAP
  const top = below + height <= window.innerHeight - EDGE ? below : Math.max(EDGE, box.top - GAP - height)
  el.style.left = `${left}px`
  el.style.top = `${top}px`
}

watch(
  () => [tip.anchor, tip.text] as const,
  ([anchor]) => {
    const el = layer.value
    if (el === null) {
      return
    }
    if (anchor === null) {
      el.hidePopover()
      return
    }
    if (!el.matches(':popover-open')) {
      el.showPopover()
    }
    place(el, anchor)
  },
  { flush: 'post' },
)
</script>

<template>
  <div ref="layer" class="tip" popover="manual" aria-hidden="true">{{ tip.text }}</div>
</template>

<style scoped>
/* 外观照搬 Fluent 2 Tooltip；最宽放到 480 px，普通文字只在空格、标点处换行，没有空格的长路径才任意断开 */
.tip {
  position: fixed;
  inset: auto;
  margin: 0;
  box-sizing: border-box;
  max-width: 480px;
  padding: 4px var(--spacingHorizontalMNudge) 6px;
  border: 1px solid var(--colorTransparentStroke);
  border-radius: var(--borderRadiusMedium);
  background: var(--colorNeutralBackground1);
  color: var(--colorNeutralForeground1);
  filter: drop-shadow(0 0 2px var(--colorNeutralShadowAmbient)) drop-shadow(0 4px 8px var(--colorNeutralShadowKey));
  font-family: var(--fontFamilyBase);
  font-size: var(--fontSizeBase200);
  line-height: var(--lineHeightBase200);
  white-space: pre-line;
  overflow-wrap: anywhere;
  pointer-events: none;
}
</style>
