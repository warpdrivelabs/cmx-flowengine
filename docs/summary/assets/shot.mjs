import { chromium } from 'playwright'
import { readdirSync, readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
const DIR = dirname(fileURLToPath(import.meta.url))
const b = await chromium.launch({ channel: 'chrome', headless: true })
const pg = await b.newContext({ deviceScaleFactor: 2 }).then((c) => c.newPage())
for (const f of readdirSync(DIR).filter((n) => /^fig-.*\.svg$/.test(n)).sort()) {
  const svg = readFileSync(join(DIR, f), 'utf8')
  const m = svg.match(/width="(\d+)" height="(\d+)"/)
  const w = +m[1], h = +m[2]
  await pg.setViewportSize({ width: w, height: h })
  await pg.setContent(`<body style="margin:0;background:#e9e9e6">${svg}</body>`)
  await pg.waitForTimeout(120)
  await pg.screenshot({ path: join(DIR, f.replace('.svg', '.png')) })
  console.log(`${f} → png ${w}x${h}`)
}
await b.close()
