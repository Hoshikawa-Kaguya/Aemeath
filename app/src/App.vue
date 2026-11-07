<script setup lang="ts">
import { useI18n } from 'vue-i18n'
import TitleBar from '@/components/TitleBar.vue'

const { t } = useI18n()
</script>

<template>
  <div class="shell">
    <TitleBar />
    <main class="content">
      <p class="empty">{{ t('shell.empty') }}</p>
    </main>
  </div>
</template>

<style scoped>
.shell {
  display: flex;
  flex-direction: column;
  height: 100vh;
}

/* 内容区是叠在 Mica 上的一层半透明底色，标题栏保持透明，让 Mica 直接透出来 */
.content {
  flex: 1;
  min-height: 0;
  display: grid;
  place-items: center;
  border-top: 1px solid var(--colorNeutralStroke2);
  border-top-left-radius: var(--borderRadiusXLarge);
  background: color-mix(in srgb, var(--colorNeutralBackground2) 70%, transparent);
}

.empty {
  color: var(--colorNeutralForeground3);
}
</style>
