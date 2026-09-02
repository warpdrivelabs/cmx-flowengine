// shot.mjs — 用本机 Google Chrome headless 把 fig-*.svg 渲成 PNG 供肉眼查版（无需 playwright）。
// 用法: node docs/report/assets/shot.mjs
import { readdirSync, readFileSync, writeFileSync, mkdirSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { execFileSync } from 'node:child_process'
const DIR = dirname(fileURLToPath(import.meta.url))
const TMP = join(DIR, '_shot'); mkdirSync(TMP, { recursive: true })
const CHROME = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome'
for (const f of readdirSync(DIR).filter((n) => /^fig-.*\.svg$/.test(n)).sort()) {
  const svg = readFileSync(join(DIR, f), 'utf8')
  const m = svg.match(/width="(\d+)" height="(\d+)"/)
  const w = +m[1], h = +m[2]
  const html = join(TMP, f.replace('.svg', '.html'))
  const png = join(DIR, f.replace('.svg', '.png'))
  writeFileSync(html, `<!doctype html><meta charset="utf-8"><body style="margin:0;background:#e9e9e6">${svg}</body>`)
  execFileSync(CHROME, ['--headless', '--disable-gpu', '--hide-scrollbars', '--force-device-scale-factor=2',
    `--window-size=${w},${h}`, `--screenshot=${png}`, `file://${html}`], { stdio: 'ignore' })
  console.log(`${f} → png ${w}x${h}`)
}
