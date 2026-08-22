// 把 assets/*.svg 逐个 base64 编码，产出可内嵌 markdown 的 data-URI 片段到 assets/embeds.json。
// 用法: node docs/summary/assets/build-embeds.mjs
import { readdirSync, readFileSync, writeFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
const dir = dirname(fileURLToPath(import.meta.url))
const out = {}
for (const f of readdirSync(dir).filter((n) => n.endsWith('.svg')).sort()) {
  const svg = readFileSync(join(dir, f))
  const b64 = svg.toString('base64')
  out[f.replace(/\.svg$/, '')] = `data:image/svg+xml;base64,${b64}`
  console.log(`${f}: ${svg.length}B svg → ${b64.length}B base64`)
}
writeFileSync(join(dir, 'embeds.json'), JSON.stringify(out, null, 2))
console.log(`\n${Object.keys(out).length} embeds → embeds.json`)
