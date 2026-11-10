<script setup lang="ts">
import { computed, useTemplateRef } from 'vue'
import { useI18n } from 'vue-i18n'
import { backend } from '@/api/backend'
import { roleOf, type Direction, type NodeView, type SlotView } from '@/api/types'
import AppIcon, { type IconName } from '@/components/AppIcon.vue'
import { formatKhz } from '@/format/format'
import { useAppStore } from '@/stores/app'
import { slotName } from '@/stores/nodeLabels'
import NodeSettingsDialog from '@/workbench/NodeSettingsDialog.vue'

// 设备页：16 个槽位的占用者、挂载状态和设备格式（ADR-0009）。
// 格式是占用节点的配置，这里用同一个节点设置对话框修改；空槽位没有可改的设置（M4 计划 F3）。
const { t } = useI18n()
const store = useAppStore()
const settingsDialog = useTemplateRef<InstanceType<typeof NodeSettingsDialog>>('settingsDialog')
/** 共享模式下跟随引擎的采样率上限（SLOT_FORMAT_MAX_HZ） */
const SLOT_FORMAT_MAX_HZ = 384_000

const slots = computed(() => store.state?.slots ?? [])
const used = computed(() => slots.value.filter((slot) => slot.occupant !== null).length)
const engineRate = computed(() => store.state?.engine.sampleRate ?? 0)
const repairing = computed(
  () => slots.value.filter((slot) => slot.health.state === 'retrying' || slot.health.state === 'failing').length,
)
const engineRunning = computed(() => store.state?.engine.state === 'running')

const sections: { direction: Direction; title: string; sub: string }[] = [
  { direction: 'in', title: 'dev.inputs', sub: 'dev.inputsSub' },
  { direction: 'out', title: 'dev.outputs', sub: 'dev.outputsSub' },
]

interface Row {
  slot: SlotView
  occupant: NodeView | undefined
}

function rows(direction: Direction): Row[] {
  return slots.value
    .filter((slot) => slot.slot.direction === direction)
    .map((slot) => ({
      slot,
      occupant: slot.occupant === null ? undefined : store.nodeById.get(slot.occupant),
    }))
}

function plugged(slot: SlotView): boolean {
  return slot.health.state === 'healthy' || slot.health.state === 'settling'
}

interface StateDisplay {
  icon: IconName
  tone: 'ok' | 'warn' | 'off'
  text: string
}

function stateOf(slot: SlotView): StateDisplay {
  const health = slot.health
  switch (health.state) {
    case 'healthy':
      return { icon: 'check', tone: 'ok', text: t('dev.stOk') }
    case 'settling':
      return { icon: 'sync', tone: 'warn', text: t('dev.stSettling') }
    case 'retrying':
      return { icon: 'sync', tone: 'warn', text: t('dev.stFix', { n: health.failures + 1 }) }
    case 'failing':
      return { icon: 'warn', tone: 'warn', text: t('dev.stFail', { n: health.failures }) }
    case 'engineOffline':
      return { icon: 'sync', tone: 'off', text: t('dev.stEngine') }
    case 'unplugged':
      return { icon: 'unplug', tone: 'off', text: t('dev.stOff') }
  }
  return { icon: 'unplug', tone: 'off', text: t('dev.stOff') }
}

/** 空槽位没有格式设置，显示"跟随引擎"：Windows 里留着的是上一个节点的格式，下一个占用者挂上时会被改掉 */
function formatText(slot: SlotView, occupant: NodeView | undefined): string {
  const sampleRate = occupant ? slot.format?.sampleRate : Math.min(engineRate.value, SLOT_FORMAT_MAX_HZ)
  const sampleFormat = occupant ? slot.format?.sampleFormat : 'int32'
  if (sampleRate === undefined || sampleFormat === undefined) {
    return t('dev.fmtUnknown')
  }
  return t('dev.fmt', { rate: formatKhz(sampleRate), format: t(`sampleFormat.${sampleFormat}`) })
}

/** 槽位读回的格式是否与占用节点的设置一致；设定是异步的，刚修改时可能暂时不一致 */
function matchesSettings(slot: SlotView, occupant: NodeView | undefined): boolean {
  if (slot.format === null || occupant?.slotSettings === null || occupant === undefined) {
    return true
  }
  const wanted = occupant.slotSettings.format
  if (wanted === null) {
    return (
      slot.format.sampleRate === Math.min(engineRate.value, SLOT_FORMAT_MAX_HZ) && slot.format.sampleFormat === 'int32'
    )
  }
  return slot.format.sampleRate === wanted.sampleRate && slot.format.sampleFormat === wanted.sampleFormat
}

function formatHint(slot: SlotView, occupant: NodeView | undefined): string {
  if (!plugged(slot)) {
    return t('dev.fmtOff')
  }
  if (!matchesSettings(slot, occupant)) {
    return t('dev.fmtMismatch')
  }
  return slot.format?.sampleRate === engineRate.value ? t('dev.fmtSame') : t('dev.fmtDiff')
}

const healthText = computed(() => {
  if (!engineRunning.value) {
    return t('dev.healthWaiting')
  }
  return repairing.value > 0 ? t('dev.healthFix', { n: repairing.value }) : t('dev.healthOk')
})
</script>

<template>
  <section class="page">
    <header>
      <h1 class="page-title">{{ t('nav.devices') }}</h1>
      <p class="page-sub">{{ t('dev.sub') }}</p>
    </header>
    <div class="body">
      <div class="summary">
        <div>
          <div class="k">{{ t('dev.driver') }}</div>
          <div class="v">
            <AppIcon
              :name="store.state?.usbipVersion ? 'check' : 'warn'"
              :class="store.state?.usbipVersion ? 'ok' : 'warn'"
            />
            <span class="ellipsis">{{
              store.state?.usbipVersion
                ? t('dev.driverVal', { version: store.state.usbipVersion })
                : t('dev.driverMissing')
            }}</span>
          </div>
        </div>
        <div>
          <div class="k">{{ t('dev.health') }}</div>
          <div class="v">
            <AppIcon
              :name="repairing > 0 || !engineRunning ? 'sync' : 'check'"
              :class="repairing > 0 || !engineRunning ? 'warn' : 'ok'"
            />
            <span class="ellipsis">{{ healthText }}</span>
          </div>
        </div>
        <div>
          <div class="k">{{ t('dev.usage') }}</div>
          <div class="v">
            <AppIcon name="slots" /><span class="ellipsis">{{ t('dev.usageVal', { used }) }}</span>
          </div>
        </div>
        <fluent-button @click="backend.checkSlots()">
          <AppIcon slot="start" name="sync" />{{ t('dev.check') }}
        </fluent-button>
      </div>

      <section v-for="section in sections" :key="section.direction" class="slot-section" :aria-label="t(section.title)">
        <h2>
          {{ t(section.title) }}<span class="caption">{{ t(section.sub) }}</span>
        </h2>
        <div class="slot-grid slot-head" aria-hidden="true">
          <span>{{ t('dev.colSlot') }}</span
          ><span>{{ t('dev.colName') }}</span
          ><span>{{ t('dev.colOcc') }}</span> <span>{{ t('dev.colState') }}</span
          ><span>{{ t('dev.colFmt') }}</span
          ><span></span>
        </div>
        <div class="slot-rows">
          <div
            v-for="{ slot, occupant } in rows(section.direction)"
            :key="slot.slot.number"
            class="slot-row slot-grid"
            :class="{ unplugged: !plugged(slot) }"
          >
            <span
              ><span class="slot-no">{{ section.direction === 'in' ? 'In' : 'Out' }} {{ slot.slot.number }}</span></span
            >
            <div class="cell">
              <b class="ellipsis" :title="slotName(slot.slot)">{{ slotName(slot.slot) }}</b>
              <span class="caption ellipsis" :title="slot.deviceName ?? ''">{{ slot.deviceName ?? '' }}</span>
            </div>
            <div v-if="occupant" class="occ" :data-role="roleOf(occupant.kind)">
              <span class="kind-ico"><AppIcon name="virt" /></span>
              <div class="cell">
                <b class="ellipsis" :title="occupant.name">{{ occupant.name }}</b>
                <span class="caption ellipsis">{{ t(`kind.${occupant.kind}`) }}</span>
              </div>
            </div>
            <div v-else class="occ none">
              <div class="cell">
                <span class="ellipsis">{{ t('dev.free') }}</span>
                <span class="caption ellipsis">{{ t('dev.freeSub') }}</span>
              </div>
            </div>
            <span class="state" :class="stateOf(slot).tone">
              <AppIcon :name="stateOf(slot).icon" /><span class="ellipsis">{{ stateOf(slot).text }}</span>
            </span>
            <div class="cell fmt">
              <span class="ellipsis">{{ formatText(slot, occupant) }}</span>
              <span class="caption ellipsis" :title="formatHint(slot, occupant)">{{ formatHint(slot, occupant) }}</span>
            </div>
            <fluent-button v-if="occupant?.slotSettings" size="small" @click="settingsDialog?.open(occupant)">
              {{ t('dev.edit') }}
            </fluent-button>
            <span v-else></span>
          </div>
        </div>
      </section>
      <p class="footnote caption">
        <AppIcon name="info" /><span>{{ t('dev.note') }}</span>
      </p>
    </div>
    <NodeSettingsDialog ref="settingsDialog" />
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
  color: var(--text-2);
}

.body {
  padding: 8px 24px 32px;
}

.summary {
  display: grid;
  grid-template-columns: repeat(3, minmax(0, 1fr)) auto;
  gap: 16px;
  align-items: center;
  padding: 14px var(--card-padding-end) 14px 16px;
  border-radius: var(--r-card);
  background: var(--card);
  border: 1px solid var(--stroke);
}

.k {
  font-size: var(--fontSizeBase200);
  color: var(--text-3);
}

.v {
  display: flex;
  align-items: center;
  gap: 6px;
  font-weight: var(--fontWeightSemibold);
  min-width: 0;
}

.ok {
  color: var(--ok);
}

.warn {
  color: var(--warn);
}

.slot-section {
  margin-top: 20px;
}

.slot-section h2 {
  display: flex;
  align-items: baseline;
  gap: 10px;
  margin: 0 0 6px;
  font: var(--fontWeightSemibold) var(--fontSizeBase400) / var(--lineHeightBase400) var(--fontFamilyBase);
}

.slot-section h2 .caption {
  font-weight: var(--fontWeightRegular);
}

/* 列宽固定：长的节点名、设备名截断，不会把某一列撑宽 */
.slot-grid {
  display: grid;
  grid-template-columns: 76px minmax(0, 1.25fr) minmax(0, 1.2fr) minmax(0, 1fr) minmax(0, 200px) 72px;
  column-gap: 12px;
  align-items: center;
}

.slot-head {
  padding: 0 var(--card-padding-end) 0 16px;
  height: 28px;
  font-size: var(--fontSizeBase200);
  color: var(--text-3);
}

.slot-rows {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

.slot-row {
  height: 56px;
  padding: 0 var(--card-padding-end) 0 16px;
  border-radius: var(--r-card);
  background: var(--card);
  border: 1px solid var(--stroke);
}

.slot-row.unplugged {
  background: transparent;
}

.slot-no {
  display: inline-flex;
  align-items: center;
  height: 24px;
  padding: 0 8px;
  border-radius: var(--borderRadiusLarge);
  background: var(--layer-alt);
  border: 1px solid var(--stroke);
  font: var(--fontWeightSemibold) var(--fontSizeBase200) / 16px var(--fontFamilyBase);
  white-space: nowrap;
}

.cell {
  display: flex;
  flex-direction: column;
  min-width: 0;
}

.cell b {
  font-weight: var(--fontWeightSemibold);
}

.occ {
  display: flex;
  align-items: center;
  gap: 8px;
  min-width: 0;
}

.occ .kind-ico {
  width: 22px;
  height: 22px;
}

.occ .kind-ico svg {
  width: 14px;
  height: 14px;
}

.occ.none {
  color: var(--text-3);
}

.state {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  min-width: 0;
}

.state.ok svg {
  color: var(--ok);
}

.state.warn svg {
  color: var(--warn);
}

.state.off {
  color: var(--text-3);
}

.footnote {
  margin-top: 16px;
  display: flex;
  gap: 8px;
  align-items: flex-start;
}

.footnote svg {
  margin-top: 2px;
}
</style>
