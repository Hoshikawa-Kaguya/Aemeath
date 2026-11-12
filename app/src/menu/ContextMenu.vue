<script setup lang="ts">
import { nextTick, onBeforeUnmount, onMounted, ref, useTemplateRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { SEPARATOR, closeMenu, openMenu, openedMenu, type MenuAction, type MenuItem } from './contextMenu'

// 唯一的右键菜单实例（Fluent 菜单样式）。另外负责全局屏蔽 WebView 自带的菜单：
// 没有组件处理的右键，在文本框里显示剪切、复制、粘贴、全选，其他地方不弹出任何菜单。
const { t } = useI18n()
const root = useTemplateRef<HTMLElement>('root')
const position = ref({ left: 0, top: 0 })
/** 菜单离窗口边缘的最小距离 */
const EDGE_MARGIN = 8

type TextField = HTMLInputElement | HTMLTextAreaElement

/** 右键点中的是不是可输入文字的框；是的话给它提供剪切、复制、粘贴菜单 */
function textFieldOf(target: EventTarget | null): TextField | null {
  if (target instanceof HTMLTextAreaElement) {
    return target
  }
  // 只有可输入文字的 input 才有文本菜单（复选框、单选框等没有）
  if (target instanceof HTMLInputElement && target.selectionStart !== null) {
    return target
  }
  return null
}

function textItems(field: TextField): MenuItem[] {
  const hasSelection = (field.selectionStart ?? 0) !== (field.selectionEnd ?? 0)
  const editable = !field.readOnly && !field.disabled
  return [
    { kind: 'action', label: t('menu.cut'), disabled: !hasSelection || !editable, run: () => command(field, 'cut') },
    { kind: 'action', label: t('menu.copy'), disabled: !hasSelection, run: () => command(field, 'copy') },
    { kind: 'action', label: t('menu.paste'), disabled: !editable, run: () => void paste(field) },
    SEPARATOR,
    { kind: 'action', label: t('menu.selectAll'), run: () => field.select() },
  ]
}

function command(field: TextField, name: 'cut' | 'copy'): void {
  field.focus()
  document.execCommand(name)
}

/** 粘贴到光标处，并触发 input 事件，让绑定这个输入框的数据同步更新 */
async function paste(field: TextField): Promise<void> {
  try {
    const text = await navigator.clipboard.readText()
    field.focus()
    field.setRangeText(text, field.selectionStart ?? 0, field.selectionEnd ?? 0, 'end')
    field.dispatchEvent(new Event('input', { bubbles: true }))
  } catch {
    // 剪贴板不可读（例如内容不是文本）：什么也不做
  }
}

/** 组件没有处理的右键（它们会 stopPropagation），落到这里 */
function onContextMenu(event: MouseEvent): void {
  const field = textFieldOf(event.target)
  if (field === null) {
    event.preventDefault()
    return
  }
  openMenu(event, textItems(field))
}

function buttons(): HTMLButtonElement[] {
  return [...(root.value?.querySelectorAll<HTMLButtonElement>('button:not([disabled])') ?? [])]
}

function focusAt(index: number): void {
  const list = buttons()
  if (list.length === 0) {
    return
  }
  list[(index + list.length) % list.length]?.focus()
}

function onKeyDown(event: KeyboardEvent): void {
  const list = buttons()
  const current = list.indexOf(document.activeElement as HTMLButtonElement)
  switch (event.key) {
    case 'ArrowDown':
      focusAt(current + 1)
      break
    case 'ArrowUp':
      focusAt(current < 0 ? list.length - 1 : current - 1)
      break
    case 'Home':
      focusAt(0)
      break
    case 'End':
      focusAt(list.length - 1)
      break
    case 'Escape':
    case 'Tab':
      closeMenu()
      break
    default:
      return
  }
  event.preventDefault()
}

function choose(item: MenuAction): void {
  closeMenu()
  item.run()
}

/** 在菜单外按下指针时关闭菜单 */
function onPointerDownOutside(event: PointerEvent): void {
  if (openedMenu.value !== null && !root.value?.contains(event.target as Node)) {
    openedMenu.value = null
  }
}

function dismiss(): void {
  openedMenu.value = null
}

/** 打开后按菜单的实际大小限制在窗口内，并把焦点放到第一项 */
watch(openedMenu, async (menu) => {
  if (menu === null) {
    return
  }
  position.value = { left: menu.x, top: menu.y }
  await nextTick()
  const rect = root.value?.getBoundingClientRect()
  if (rect !== undefined) {
    position.value = {
      left: Math.max(EDGE_MARGIN, Math.min(menu.x, window.innerWidth - rect.width - EDGE_MARGIN)),
      top: Math.max(EDGE_MARGIN, Math.min(menu.y, window.innerHeight - rect.height - EDGE_MARGIN)),
    }
  }
  focusAt(0)
})

onMounted(() => {
  window.addEventListener('contextmenu', onContextMenu)
  window.addEventListener('pointerdown', onPointerDownOutside, true)
  window.addEventListener('blur', dismiss)
  window.addEventListener('resize', dismiss)
})
onBeforeUnmount(() => {
  window.removeEventListener('contextmenu', onContextMenu)
  window.removeEventListener('pointerdown', onPointerDownOutside, true)
  window.removeEventListener('blur', dismiss)
  window.removeEventListener('resize', dismiss)
})
</script>

<template>
  <Transition name="menu">
    <div
      v-if="openedMenu"
      ref="root"
      class="menu"
      role="menu"
      :style="{ left: `${position.left}px`, top: `${position.top}px` }"
      @keydown="onKeyDown"
      @contextmenu.prevent.stop
      @wheel.prevent
    >
      <template v-for="(item, index) in openedMenu.items" :key="index">
        <div v-if="item.kind === 'separator'" class="sep" role="separator"></div>
        <button
          v-else
          type="button"
          role="menuitem"
          class="item ellipsis"
          :class="{ danger: item.danger }"
          :disabled="item.disabled"
          @click="choose(item)"
        >
          {{ item.label }}
        </button>
      </template>
    </div>
  </Transition>
</template>

<style scoped>
/* Fluent 2 的菜单：圆角卡片、阴影 16、菜单项高 32 px */
.menu {
  position: fixed;
  z-index: 100;
  display: flex;
  flex-direction: column;
  width: 220px;
  padding: 4px;
  border-radius: var(--r-card);
  background: var(--card-solid);
  border: 1px solid var(--stroke);
  box-shadow: var(--shadow16);
  animation: menu-in var(--dur) var(--ease);
}

.item {
  display: block;
  height: 32px;
  padding: 0 10px;
  border: 0;
  border-radius: var(--r-ctl);
  background: transparent;
  color: var(--text);
  font: var(--fontSizeBase300) / 32px var(--fontFamilyBase);
  text-align: left;
  cursor: default;
}

.item:hover:not(:disabled),
.item:focus-visible {
  background: var(--card-hover);
  outline: none;
}

.item:disabled {
  color: var(--text-dis);
}

.item.danger:not(:disabled) {
  color: var(--crit);
}

.sep {
  height: 1px;
  margin: 4px -4px;
  background: var(--stroke);
}

/* 收起比弹出快；收起期间不再响应点击 */
.menu-leave-active {
  pointer-events: none;
  transition:
    opacity var(--dur-fast) var(--ease-exit),
    transform var(--dur-fast) var(--ease-exit);
}

.menu-leave-to {
  opacity: 0;
  transform: translateY(-4px);
}

@keyframes menu-in {
  from {
    opacity: 0;
    transform: translateY(-4px);
  }
}

@media (prefers-reduced-motion: reduce) {
  .menu {
    animation: none;
  }
}
</style>
