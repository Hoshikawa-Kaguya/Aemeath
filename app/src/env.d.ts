/// <reference types="vite/client" />

declare module 'vue' {
  interface GlobalDirectives {
    vTip: (typeof import('./tip/tip'))['vTip']
  }
}

export {}
