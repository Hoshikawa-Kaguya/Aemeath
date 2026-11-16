// 门限滑块（节点设置与控制台共用）：滑块最低一格是"关"，其余对应 −80 到 0 dBFS（GLOSSARY：门限）。

/** 后端允许的阈值范围（aemeath_protocol::units::GateThreshold） */
export const GATE_MIN_DB = -80
export const GATE_MAX_DB = 0
/** 阈值的步进（dB）：滑块、滚轮都按它调 */
export const GATE_STEP_DB = 0.5
/** 滑块上表示"关"的位置：比最低阈值再低一格 */
export const GATE_OFF_POSITION = GATE_MIN_DB - GATE_STEP_DB

/** 阈值换算成滑块位置；没有门限时在"关"的位置 */
export function gateToPosition(gateDb: number | null): number {
  return gateDb ?? GATE_OFF_POSITION
}

/** 滑块位置换算成阈值（对齐到步进）；在"关"的位置时返回 null */
export function positionToGate(position: number): number | null {
  if (position <= GATE_OFF_POSITION) {
    return null
  }
  const snapped = Math.round(position / GATE_STEP_DB) * GATE_STEP_DB
  return Math.min(GATE_MAX_DB, Math.max(GATE_MIN_DB, snapped))
}
