// 门限滑块（节点设置与控制台共用）：滑块最左端一格是"关"，其余对应 −80 到 0 dBFS（GLOSSARY：门限）。

/** 后端允许的阈值范围（aemeath_protocol::units::GateThreshold） */
export const GATE_MIN_DB = -80
export const GATE_MAX_DB = 0
/** 滑块上表示"关"的位置：比最低阈值再低一格 */
export const GATE_OFF_POSITION = GATE_MIN_DB - 1

export function gateToPosition(gateDb: number | null): number {
  return gateDb ?? GATE_OFF_POSITION
}

export function positionToGate(position: number): number | null {
  return position <= GATE_OFF_POSITION ? null : Math.min(GATE_MAX_DB, Math.max(GATE_MIN_DB, Math.round(position)))
}
