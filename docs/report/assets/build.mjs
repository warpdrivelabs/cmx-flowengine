// build.mjs — 把 report.template.md 里的 {{FIG:name}} 占位替换成 base64 内嵌 <img>，产出自包含终稿。
// 用法: node docs/report/assets/build.mjs
import { readFileSync, writeFileSync, existsSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
const DIR = dirname(fileURLToPath(import.meta.url))               // docs/report/assets
const REPORT = join(DIR, '..')                                    // docs/report
const TEMPLATE = join(REPORT, 'report.template.md')
const OUT = join(REPORT, 'cmx-flowengine-实现方案与开源全景对比.md')

const FIG = /\{\{FIG:([a-z0-9-]+)\}\}/g

function img (name) {
  const svgPath = join(DIR, `fig-${name}.svg`)
  if (!existsSync(svgPath)) throw new Error(`missing svg: ${name}`)
  const svg = readFileSync(svgPath)
  const m = svg.toString('utf8').match(/width="(\d+)"/)
  const maxw = Math.min(m ? +m[1] : 960, 1000)
  const b64 = svg.toString('base64')
  return `<p align="center"><img alt="${name}" style="width:100%;max-width:${maxw}px" src="data:image/svg+xml;base64,${b64}"/></p>`
}

let t = readFileSync(TEMPLATE, 'utf8')
const used = []
t = t.replace(FIG, (_, name) => { used.push(name); return img(name) })
const leftover = t.match(FIG)
if (leftover) throw new Error('unresolved placeholders: ' + leftover.join(','))
writeFileSync(OUT, t)
console.log(`wrote ${OUT} (${(Buffer.byteLength(t) / 1024).toFixed(0)} KiB); embedded ${used.length} figs: ${used.join(', ')}`)
