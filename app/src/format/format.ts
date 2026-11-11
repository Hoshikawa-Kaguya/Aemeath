// 数值的显示格式。

/** 负号用 U+2212：比连字符更长，数字列里更容易看清正负 */
const MINUS = '−'
const KHZ = 1000
const SECONDS_PER_MINUTE = 60
const SECONDS_PER_HOUR = 3600
const BYTES_PER_KB = 1024
const BYTE_UNITS = ['B', 'KB', 'MB', 'GB', 'TB']

/** 增益：0 dB、+3 dB、−6 dB；最多保留一位小数 */
export function formatGain(db: number): string {
  const rounded = Math.round(db * 10) / 10
  if (rounded === 0) {
    return '0 dB'
  }
  const sign = rounded > 0 ? '+' : MINUS
  return `${sign}${Math.abs(rounded)} dB`
}

/** 门限阈值：−45 dBFS（取整） */
export function formatThreshold(db: number): string {
  const rounded = Math.round(db)
  return rounded === 0 ? '0 dBFS' : `${MINUS}${Math.abs(rounded)} dBFS`
}

/** 时长：65 → "01:05"，3665 → "1:01:05" */
export function formatDuration(seconds: number): string {
  const total = Math.floor(seconds)
  const hours = Math.floor(total / SECONDS_PER_HOUR)
  const minutes = Math.floor((total % SECONDS_PER_HOUR) / SECONDS_PER_MINUTE)
  const rest = String(total % SECONDS_PER_MINUTE).padStart(2, '0')
  const clock = `${String(minutes).padStart(2, '0')}:${rest}`
  return hours > 0 ? `${hours}:${clock}` : clock
}

/** 文件大小：按 1024 换算，保留一位小数 */
export function formatBytes(bytes: number): string {
  let value = bytes
  let unit = 0
  while (value >= BYTES_PER_KB && unit < BYTE_UNITS.length - 1) {
    value /= BYTES_PER_KB
    unit += 1
  }
  return `${unit === 0 ? value : value.toFixed(1)} ${BYTE_UNITS[unit]}`
}

/** 采样率：48000 → "48"，44100 → "44.1" */
export function formatKhz(hz: number): string {
  return String(Math.round((hz / KHZ) * 10) / 10)
}
