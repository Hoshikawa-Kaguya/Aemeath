<!-- 图标集（取自界面原型），在 App 中放置一次；各处用 <AppIcon name="…"> 引用。线条色跟随 currentColor。 -->
<template>
  <svg width="0" height="0" style="position: absolute" aria-hidden="true">
    <defs>
      <symbol id="i-graph" viewBox="0 0 24 24">
        <rect x="3" y="4" width="6" height="5" rx="1.5" />
        <rect x="15" y="15" width="6" height="5" rx="1.5" />
        <rect x="15" y="4" width="6" height="5" rx="1.5" />
        <path d="M9 6.5h6M9 6.5c3 0 3 11 6 11" />
      </symbol>
      <symbol id="i-slots" viewBox="0 0 24 24">
        <rect x="3" y="4" width="18" height="6" rx="1.5" />
        <rect x="3" y="14" width="18" height="6" rx="1.5" />
        <path d="M7 7h.01M7 17h.01M11 7h6M11 17h6" />
      </symbol>
      <symbol id="i-gear" viewBox="0 0 24 24">
        <circle cx="12" cy="12" r="3" />
        <path
          d="M12 2.5v3M12 18.5v3M2.5 12h3M18.5 12h3M5.3 5.3l2.1 2.1M16.6 16.6l2.1 2.1M5.3 18.7l2.1-2.1M16.6 7.4l2.1-2.1"
        />
      </symbol>
      <symbol id="i-mic" viewBox="0 0 24 24">
        <rect x="9" y="3" width="6" height="11" rx="3" />
        <path d="M5.5 11a6.5 6.5 0 0 0 13 0M12 17.5V21" />
      </symbol>
      <symbol id="i-spk" viewBox="0 0 24 24">
        <path d="M4 9.5h3.5L12 5v14l-4.5-4.5H4z" />
        <path d="M15.5 9a4 4 0 0 1 0 6M18 6.5a7.5 7.5 0 0 1 0 11" />
      </symbol>
      <symbol id="i-bus" viewBox="0 0 24 24"><path d="M4 6h5l4 6-4 6H4M13 12h7M17 9l3 3-3 3" /></symbol>
      <symbol id="i-virt" viewBox="0 0 24 24">
        <path d="M4 12h4M16 12h4M8 8h8v8H8z" />
        <path d="M11 11h2v2h-2z" />
      </symbol>
      <symbol id="i-warn" viewBox="0 0 24 24">
        <path d="M12 3.5 2.5 20h19z" />
        <path d="M12 10v4.5M12 17.2v.3" />
      </symbol>
      <symbol id="i-x" viewBox="0 0 24 24"><path d="M6 6l12 12M18 6 6 18" /></symbol>
      <symbol id="i-plus" viewBox="0 0 24 24"><path d="M12 5v14M5 12h14" /></symbol>
      <symbol id="i-fit" viewBox="0 0 24 24"><path d="M4 9V4h5M15 4h5v5M20 15v5h-5M9 20H4v-5" /></symbol>
      <symbol id="i-check" viewBox="0 0 24 24">
        <circle cx="12" cy="12" r="8.5" />
        <path d="m8 12.5 2.7 2.7L16.5 9" />
      </symbol>
      <symbol id="i-unplug" viewBox="0 0 24 24">
        <path d="M9 3v4M15 3v4M7 7h10v3a5 5 0 0 1-10 0zM12 15v6" />
        <path d="M3 3l18 18" />
      </symbol>
      <symbol id="i-sync" viewBox="0 0 24 24">
        <path d="M20 11a8 8 0 0 0-14.3-4.9L4 8M4 4v4h4M4 13a8 8 0 0 0 14.3 4.9L20 16M20 20v-4h-4" />
      </symbol>
      <symbol id="i-info" viewBox="0 0 24 24">
        <circle cx="12" cy="12" r="8.5" />
        <path d="M12 11v5M12 8v.01" />
      </symbol>
      <symbol id="i-lang" viewBox="0 0 24 24">
        <circle cx="12" cy="12" r="8.5" />
        <path d="M3.5 12h17M12 3.5c2.5 2.5 3.5 5.5 3.5 8.5s-1 6-3.5 8.5c-2.5-2.5-3.5-5.5-3.5-8.5s1-6 3.5-8.5z" />
      </symbol>
      <symbol id="i-undo" viewBox="0 0 24 24"><path d="M9 14 4 9l5-5M4 9h10a6 6 0 0 1 0 12h-3" /></symbol>
      <symbol id="i-rec" viewBox="0 0 24 24"><circle cx="12" cy="12" r="6" fill="currentColor" stroke="none" /></symbol>
      <symbol id="i-stop" viewBox="0 0 24 24">
        <rect x="7" y="7" width="10" height="10" rx="1.5" fill="currentColor" stroke="none" />
      </symbol>
      <symbol id="i-clock" viewBox="0 0 24 24">
        <circle cx="12" cy="12" r="8.5" />
        <path d="M12 7.5V12l3 2" />
      </symbol>
      <symbol id="i-key" viewBox="0 0 24 24">
        <rect x="3" y="7" width="18" height="11" rx="2" />
        <path d="M7 11h.01M11 11h.01M15 11h.01M8 15h8" />
      </symbol>
      <symbol id="i-fx" viewBox="0 0 24 24"><path d="M3 12h4l3-6h4l3 6h4M10 18h4" /></symbol>
      <symbol id="i-apps" viewBox="0 0 24 24">
        <rect x="4" y="4" width="6.5" height="6.5" rx="1.5" />
        <rect x="13.5" y="4" width="6.5" height="6.5" rx="1.5" />
        <rect x="4" y="13.5" width="6.5" height="6.5" rx="1.5" />
        <rect x="13.5" y="13.5" width="6.5" height="6.5" rx="1.5" />
      </symbol>
      <symbol id="i-tools" viewBox="0 0 24 24">
        <path d="M14.5 6.5a4 4 0 0 0-5.3 5.3L4 17l3 3 5.2-5.2a4 4 0 0 0 5.3-5.3l-2.6 2.6-2.4-.6-.6-2.4z" />
      </symbol>
      <symbol id="i-more" viewBox="0 0 24 24">
        <circle cx="6" cy="12" r="1.4" fill="currentColor" stroke="none" />
        <circle cx="12" cy="12" r="1.4" fill="currentColor" stroke="none" />
        <circle cx="18" cy="12" r="1.4" fill="currentColor" stroke="none" />
      </symbol>
      <symbol id="i-grip" viewBox="0 0 10 14">
        <circle cx="3" cy="3" r="1" fill="currentColor" stroke="none" />
        <circle cx="7" cy="3" r="1" fill="currentColor" stroke="none" />
        <circle cx="3" cy="7" r="1" fill="currentColor" stroke="none" />
        <circle cx="7" cy="7" r="1" fill="currentColor" stroke="none" />
        <circle cx="3" cy="11" r="1" fill="currentColor" stroke="none" />
        <circle cx="7" cy="11" r="1" fill="currentColor" stroke="none" />
      </symbol>
    </defs>
  </svg>
</template>
