// fluent-dialog 把目标为 <dialog> 本身的 click 当作点了遮罩而关闭。按下和松开的位置不同时（例如在输入框里拖选文字拖到遮罩上），
// click 落在两者共同的祖先 <dialog> 上，也会被关掉。这里只放行按下和松开都在遮罩上的 click。
let pressedOn: EventTarget | null = null
let releasedOn: EventTarget | null = null

function onPointerDown(event: PointerEvent): void {
  pressedOn = event.composedPath()[0] ?? null
}

function onPointerUp(event: PointerEvent): void {
  releasedOn = event.composedPath()[0] ?? null
}

function onClick(event: MouseEvent): void {
  const target = event.composedPath()[0]
  if (
    target instanceof HTMLDialogElement &&
    target.getRootNode() instanceof ShadowRoot &&
    (target !== pressedOn || target !== releasedOn)
  ) {
    event.stopPropagation()
  }
}

/** 安装全局监听，应用启动时调用一次 */
export function installDialogDismissFix(): void {
  window.addEventListener('pointerdown', onPointerDown, true)
  window.addEventListener('pointerup', onPointerUp, true)
  window.addEventListener('click', onClick, true)
}

/** fluent-dialog 关闭时发出的 toggle 事件；点遮罩、按 Esc 和代码调用 hide() 都会发 */
export function isClosing(event: Event): boolean {
  return (event as CustomEvent<{ newState?: string } | undefined>).detail?.newState === 'closed'
}
