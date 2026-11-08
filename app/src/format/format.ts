// 数值的显示格式。

/** 负号用 U+2212，与原型一致（比连字符更易读） */
const MINUS = '−'
const KHZ = 1000

/** 增益：0 dB、+3 dB、−6 dB；最多保留一位小数 */
export function formatGain(db: number): string {
  const rounded = Math.round(db * 10) / 10
  if (rounded === 0) {
    return '0 dB'
  }
  const sign = rounded > 0 ? '+' : MINUS
  return `${sign}${Math.abs(rounded)} dB`
}

/** 采样率：48000 → "48"，44100 → "44.1" */
export function formatKhz(hz: number): string {
  return String(Math.round((hz / KHZ) * 10) / 10)
}
