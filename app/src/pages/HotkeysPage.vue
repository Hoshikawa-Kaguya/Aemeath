<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import type { HotkeyRowView, HotkeyTargetView } from '@/api/types'
import { useAppStore } from '@/stores/app'
import HotkeyField from '@/workbench/HotkeyField.vue'
import { describeTarget } from '@/workbench/hotkeyLabels'

// 热键页：分"全局"和"专属"两组，每行可以直接修改，与节点设置里改的是同一份数据。
const { t } = useI18n()
const store = useAppStore()

const GLOBAL_TARGETS: HotkeyTargetView[] = [{ type: 'allRecording' }, { type: 'panicMute' }, { type: 'toggleWindow' }]

const rows = computed<HotkeyRowView[]>(() => store.state?.hotkeys ?? [])

function isGlobal(target: HotkeyTargetView): boolean {
  return target.type === 'allRecording' || target.type === 'panicMute' || target.type === 'toggleWindow'
}

function rowOf(target: HotkeyTargetView): HotkeyRowView | undefined {
  return rows.value.find((row) => row.target.type === target.type)
}

function toggleOnly(target: HotkeyTargetView): boolean {
  return target.type === 'allRecording' || target.type === 'toggleWindow' || target.type === 'nodeRecord'
}

const dedicated = computed(() => rows.value.filter((row) => !isGlobal(row.target)))
</script>

<template>
  <section class="page">
    <header>
      <h1 class="page-title">{{ t('nav.hotkeys') }}</h1>
      <p class="page-sub">{{ t('hotkey.sub') }}</p>
    </header>
    <div class="body">
      <h2 class="group">{{ t('hotkey.global') }}</h2>
      <div class="cardlist">
        <div v-for="target in GLOBAL_TARGETS" :key="target.type" class="card-row">
          <b class="ellipsis">{{ describeTarget(target, store.nodes) }}</b>
          <HotkeyField
            :target="target"
            :binding="rowOf(target)?.binding ?? null"
            :occupied="rowOf(target)?.occupied"
            :toggle-only="toggleOnly(target)"
          />
        </div>
      </div>

      <h2 class="group">{{ t('hotkey.dedicated') }}</h2>
      <p v-if="dedicated.length === 0" class="caption">{{ t('hotkey.dedicatedEmpty') }}</p>
      <div class="cardlist">
        <div v-for="row in dedicated" :key="JSON.stringify(row.target)" class="card-row">
          <b v-tip="describeTarget(row.target, store.nodes)" class="ellipsis">
            {{ describeTarget(row.target, store.nodes) }}
          </b>
          <HotkeyField
            :target="row.target"
            :binding="row.binding"
            :occupied="row.occupied"
            :toggle-only="toggleOnly(row.target)"
          />
        </div>
      </div>
      <p class="caption note">{{ t('hotkey.note') }}</p>
    </div>
  </section>
</template>

<style scoped>
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

.group {
  margin: 16px 0 8px;
  font: var(--fontWeightSemibold) var(--fontSizeBase400) / var(--lineHeightBase400) var(--fontFamilyBase);
}

.cardlist {
  display: flex;
  flex-direction: column;
  gap: 4px;
}

/* 列宽固定，长节点名截断 */
.card-row {
  display: grid;
  grid-template-columns: minmax(0, 1fr) minmax(0, 1.2fr);
  align-items: center;
  gap: 12px;
  min-height: 52px;
  padding: 8px var(--card-padding-end) 8px 16px;
  border-radius: var(--r-card);
  background: var(--card);
  border: 1px solid var(--stroke);
}

.card-row b {
  font-weight: var(--fontWeightSemibold);
}

.note {
  margin-top: 16px;
}
</style>
