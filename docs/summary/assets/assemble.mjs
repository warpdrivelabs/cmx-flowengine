// 把 summary.template.md 的 @@fig-key@@ 占位替换为 embeds.json 的 base64 data-URI，产出自包含最终文档。
import { readFileSync, writeFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
const A = dirname(fileURLToPath(import.meta.url))
const SUM = dirname(A)
const embeds = JSON.parse(readFileSync(join(A, 'embeds.json'), 'utf8'))
let md = readFileSync(join(SUM, 'summary.template.md'), 'utf8')
let n = 0
md = md.replace(/@@([a-z0-9-]+)@@/g, (m, k) => {
  if (!embeds[k]) throw new Error('missing embed: ' + k)
  n++; return embeds[k]
})
const leftover = md.match(/@@[a-z0-9-]+@@/g)
if (leftover) throw new Error('unresolved placeholders: ' + leftover.join(','))
// 输出文件名可经 argv[2] 指定；缺省用基名。传相对名即写进 docs/summary/。
const outName = process.argv[2] || 'cmx-flowengine-阶段性总结.md'
const out = join(SUM, outName)
writeFileSync(out, md)
console.log(`substituted ${n} embeds → ${out} (${(md.length / 1024).toFixed(0)} KB)`)
