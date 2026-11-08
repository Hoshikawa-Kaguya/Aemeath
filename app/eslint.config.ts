import { globalIgnores } from 'eslint/config'
import { defineConfigWithVueTs, vueTsConfigs } from '@vue/eslint-config-typescript'
import pluginVue from 'eslint-plugin-vue'
import skipFormatting from '@vue/eslint-config-prettier/skip-formatting'

// 格式交给 Prettier，ESLint 只管代码问题
export default defineConfigWithVueTs(
  {
    name: 'app/files-to-lint',
    files: ['**/*.{ts,mts,vue}'],
  },
  globalIgnores(['**/dist/**', 'src-tauri/**', 'crates/**']),
  pluginVue.configs['flat/recommended'],
  vueTsConfigs.recommended,
  skipFormatting,
  {
    rules: {
      // Fluent 2 的 Web Components 用原生的 slot 属性放置图标和按钮（例如 <fluent-button> 的 start），
      // 这不是 Vue 已弃用的插槽语法
      'vue/no-deprecated-slot-attribute': ['error', { ignoreParents: ['/^fluent-/'] }],
    },
  },
)
