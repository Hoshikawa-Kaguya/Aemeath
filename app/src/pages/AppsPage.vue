<script setup lang="ts">
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { backend } from '@/api/backend'
import type { AppView, AppsView, LandingChoice } from '@/api/types'
import AppIcon from '@/components/AppIcon.vue'
import { useAppStore } from '@/stores/app'
import { usePolling } from './usePolling'

// 应用页（选型报告 5.5 节、S7）：只列出有音频会话的程序，可以把落点（GLOSSARY：落点）改到某个虚拟输入、外部输出，
// 或者跟随系统默认。指派按可执行文件持久保存；已在播放的流不一定转移，这时提示"重启该应用后生效"。
// 被 Aemeath 独占的外部输出不可选（应用打不开它）。每张卡片可以把程序添加为应用源（M3 计划 A5）。
const REFRESH_MS = 3000
/** 指派后等这么久再刷新，看会话有没有转移 */
const SETTLE_MS = 800
const FOLLOW_SYSTEM = ''
const NEW_NODE_X = 40
const NEW_NODE_Y = 40

const { t } = useI18n()
const store = useAppStore()
const view = ref<AppsView | null>(null)
const failed = ref(false)
/** 刚指派、还在等会话转移的进程及选中的值：下拉框立即显示新值，这段时间不提示"重启后生效"，也不用轮询结果覆盖 */
const settling = ref(new Map<number, string>())

/** `settled` 是等待结束的进程：拿到新列表后才解除，否则下拉框会先退回旧值 */
async function refresh(settled?: number): Promise<void> {
  let next: AppsView | null = null
  try {
    next = await backend.listApps()
    failed.value = false
  } catch {
    failed.value = true
  }
  if (settled !== undefined) {
    settling.value.delete(settled)
  }
  if (next !== null && settling.value.size === 0) {
    view.value = next
  }
}

usePolling(() => refresh(), REFRESH_MS)

const virtualInputs = computed(() => view.value?.choices.filter((choice) => choice.kind === 'virtualInput') ?? [])
const outputs = computed(() => view.value?.choices.filter((choice) => choice.kind === 'externalOutput') ?? [])

function choiceOf(endpointId: string): LandingChoice | undefined {
  return view.value?.choices.find((choice) => choice.endpointId === endpointId)
}

function landingName(app: AppView): string {
  const choice = choiceOf(app.landing)
  return choice === undefined ? t('apps.unknownDevice') : choice.name
}

/** 下拉框显示的值：Aemeath 记录的指派优先，否则是实际落点（不在候选里时为"跟随系统默认"） */
function selectedOf(app: AppView): string {
  const chosen = settling.value.get(app.processId)
  if (chosen !== undefined) {
    return chosen
  }
  if (app.assigned !== null) {
    return app.assigned
  }
  const choice = choiceOf(app.landing)
  return choice !== undefined && choice.kind !== 'other' ? app.landing : FOLLOW_SYSTEM
}

/** 指派了，但会话还在原来的端点上：多半是程序启动时就选定了设备 */
function needsRestart(app: AppView): boolean {
  return !settling.value.has(app.processId) && app.assigned !== null && app.assigned !== app.landing
}

function optionLabel(choice: LandingChoice): string {
  const name =
    choice.detail === null ? choice.name : t('apps.virtualInput', { node: choice.name, device: choice.detail })
  return choice.exclusiveByAemeath ? t('apps.exclusive', { name }) : name
}

async function choose(app: AppView, event: Event): Promise<void> {
  const value = (event.target as HTMLSelectElement).value
  const processId = app.processId
  settling.value.set(processId, value)
  try {
    await backend.setLanding(processId, app.executable, value === FOLLOW_SYSTEM ? null : value)
  } catch (error) {
    store.showToast(store.errorText(error))
  }
  window.setTimeout(() => void refresh(processId), SETTLE_MS)
}

/** 添加为应用源：放在工作台左上角，用户再拖到合适的位置 */
async function addSource(app: AppView): Promise<void> {
  if (app.executable === null) {
    return
  }
  const program = app.executable
  try {
    const name = await backend.describeProgram(program)
    if (await store.run(() => backend.addNode({ type: 'appSource', program }, name, NEW_NODE_X, NEW_NODE_Y))) {
      await refresh()
    }
  } catch (error) {
    store.showToast(store.errorText(error))
  }
}
</script>

<template>
  <section class="page">
    <header>
      <h1 class="page-title">{{ t('nav.apps') }}</h1>
      <p class="page-sub">{{ t('apps.sub') }}</p>
    </header>
    <div class="body">
      <p v-if="failed" class="caption">{{ t('apps.unavailable') }}</p>
      <p v-else-if="view !== null && view.apps.length === 0" class="caption">{{ t('apps.empty') }}</p>
      <div class="cardlist">
        <div v-for="app in view?.apps ?? []" :key="`${app.processId}-${app.system}`" class="card-row">
          <span class="ico"><AppIcon :name="app.system ? 'spk' : 'apps'" /></span>
          <div class="who">
            <b class="ellipsis" :title="app.system ? t('apps.system') : app.name">
              {{ app.system ? t('apps.system') : app.name }}
            </b>
            <span class="caption ellipsis" :title="app.executable ?? ''">{{ app.executable ?? '' }}</span>
            <div v-if="!app.system && app.executable !== null">
              <button v-if="app.sourceNode === null" type="button" class="link" @click="addSource(app)">
                {{ t('apps.addSource') }}
              </button>
              <button v-else type="button" class="link" @click="store.page = 'workbench'">
                {{ t('apps.showSource') }}
              </button>
            </div>
          </div>
          <div class="now">
            <span class="caption">{{ t('apps.now') }}</span>
            <span class="ellipsis" :title="landingName(app)">{{ landingName(app) }}</span>
          </div>
          <div class="pick">
            <select
              class="select"
              :value="selectedOf(app)"
              :disabled="app.system || settling.has(app.processId)"
              :aria-label="t('apps.landingAria', { app: app.system ? t('apps.system') : app.name })"
              @change="choose(app, $event)"
            >
              <option :value="FOLLOW_SYSTEM">{{ t('apps.followSystem') }}</option>
              <optgroup v-if="virtualInputs.length > 0" :label="t('kind.virtualInput')">
                <option v-for="choice in virtualInputs" :key="choice.endpointId" :value="choice.endpointId">
                  {{ optionLabel(choice) }}
                </option>
              </optgroup>
              <optgroup v-if="outputs.length > 0" :label="t('kind.externalOutput')">
                <option
                  v-for="choice in outputs"
                  :key="choice.endpointId"
                  :value="choice.endpointId"
                  :disabled="choice.exclusiveByAemeath"
                >
                  {{ optionLabel(choice) }}
                </option>
              </optgroup>
            </select>
            <span v-if="app.system" class="caption">{{ t('apps.systemHint') }}</span>
            <span v-else-if="settling.has(app.processId)" class="caption" role="status">{{ t('apps.assigning') }}</span>
            <span v-else-if="needsRestart(app)" class="caption warn">
              <AppIcon name="warn" />{{ t('apps.restart') }}
            </span>
          </div>
        </div>
      </div>
      <p class="caption note">{{ t('apps.note') }}</p>
    </div>
  </section>
</template>

<style scoped>
/* 整页一起滚动，滚动条占满内容区的全部高度 */
.page {
  height: 100%;
  overflow: auto;
}

.page-title {
  font: var(--fontWeightSemibold) var(--fontSizeHero700) / var(--lineHeightHero700) var(--fontFamilyBase);
  margin: 20px 24px 4px;
}

.page-sub {
  margin: 0 24px 8px;
  max-width: 900px;
  color: var(--text-2);
}

.body {
  padding: 8px 24px 32px;
}

.cardlist {
  display: flex;
  flex-direction: column;
  gap: 4px;
  max-width: 1000px;
}

/* 列宽固定，长程序名、长设备名都截断 */
.card-row {
  display: grid;
  grid-template-columns: 36px minmax(0, 1.3fr) minmax(0, 1fr) minmax(0, 1.2fr);
  align-items: center;
  gap: 12px;
  min-height: 64px;
  padding: 10px 16px;
  border-radius: var(--r-card);
  background: var(--card);
  border: 1px solid var(--stroke);
}

.ico {
  display: grid;
  place-items: center;
  width: 32px;
  height: 32px;
  border-radius: 6px;
  background: var(--layer-alt);
  color: var(--text-2);
}

.ico svg {
  width: 18px;
  height: 18px;
}

.who,
.now,
.pick {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}

.who b {
  font-weight: var(--fontWeightSemibold);
}

.pick .select {
  width: 100%;
}

.link {
  height: 22px;
  padding: 0 4px;
  margin-left: -4px;
  border: 0;
  border-radius: var(--r-ctl);
  background: transparent;
  color: var(--accent-text);
  font: var(--fontSizeBase100) / 22px var(--fontFamilyBase);
  cursor: pointer;
}

.link:hover {
  background: var(--card-hover);
}

.warn {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  color: var(--text);
}

.warn svg {
  width: 14px;
  height: 14px;
  color: var(--warn);
}

.note {
  max-width: 900px;
  margin-top: 16px;
}
</style>
