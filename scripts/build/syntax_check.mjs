// Parse every module of a decompiled bundle and report the ones a JavaScript
// engine rejects.
//
// The round trip corpus re-encodes bytecode, so it proves the decoder reads a
// file correctly and says nothing about the JavaScript that comes out the other
// end. A module that does not parse is worthless to whoever reads it, and that
// went unmeasured until this script: 16 percent of the modules of a Discord
// build failed to parse while every gate was green.
//
// Usage:
//   node --experimental-vm-modules scripts/build/syntax_check.mjs out.js [--max N]
//
// Exits non zero when more than N modules fail (N defaults to 0).

import fs from 'node:fs';
import readline from 'node:readline';
import vm from 'node:vm';

const args = process.argv.slice(2);
const file = args.find((a) => !a.startsWith('--'));
const maxIdx = args.indexOf('--max');
const max = maxIdx === -1 ? 0 : Number(args[maxIdx + 1] ?? 0);

if (!file) {
  console.error('usage: syntax_check.mjs <decompiled.js> [--max N]');
  process.exit(2);
}

const rl = readline.createInterface({
  input: fs.createReadStream(file),
  crlfDelay: Infinity,
});

let lines = [];
let id = null;
let checked = 0;
let failed = 0;
let jsx = 0;
const families = new Map();
const samples = [];

const dumpDir = (() => {
  const i = process.argv.indexOf('--dump');
  return i === -1 ? null : process.argv[i + 1];
})();
if (dumpDir) fs.mkdirSync(dumpDir, { recursive: true });

// An opening element plus a self closing or matching closing form. Requiring
// both keeps a stray comparison such as `a < b` from passing for markup.
function looksLikeJsx(source) {
  const opens = /<[A-Za-z_$][\w$.]*[\s/>]/.test(source);
  const closes = /\/>|<\/[A-Za-z_$][\w$.]*>/.test(source);
  return opens && closes;
}

function check(moduleId, body) {
  if (moduleId === null || body.length === 0) return;
  checked++;
  try {
    new vm.SourceTextModule(body.join('\n'), { identifier: `m${moduleId}` });
  } catch (err) {
    const message = String(err.message).slice(0, 100);
    const source = body.join('\n');

    // JSX is deliberate output, and a plain JavaScript parser rejects it by
    // construction, so counting it as a failure buries the real defects. It is
    // set aside only when the engine choked on `<` and the module really does
    // carry an element, opening tag and closing form both.
    if (message.includes("Unexpected token '<'") && looksLikeJsx(source)) {
      jsx++;
      return;
    }

    failed++;
    // Fold the varying parts away so the families stay countable.
    const family = message.replace(/'[^']*'/g, "'X'").replace(/\d+/g, 'N');
    families.set(family, (families.get(family) ?? 0) + 1);
    if (samples.length < 10) samples.push({ moduleId, message });
    // `node --check` on a dumped file reports the offending line and a caret,
    // which the thrown error does not carry.
    if (dumpDir) fs.writeFileSync(`${dumpDir}/m${moduleId}.mjs`, source);
  }
}

for await (const line of rl) {
  const header = /^\/\/ === Module (\d+)/.exec(line);
  if (header) {
    check(id, lines);
    id = header[1];
    lines = [];
    continue;
  }
  if (id !== null) lines.push(line);
}
check(id, lines);

const pct = checked === 0 ? 0 : (100 * failed) / checked;
console.log(`modules=${checked}  parse_fail=${failed}  (${pct.toFixed(2)}%)  jsx=${jsx}`);

if (families.size > 0) {
  console.log('\nfailure families:');
  for (const [family, n] of [...families].sort((a, b) => b[1] - a[1])) {
    console.log(`  ${String(n).padStart(6)}  ${family}`);
  }
  console.log('\nsamples:');
  for (const s of samples) console.log(`  module ${s.moduleId}: ${s.message}`);
}

if (failed > max) {
  console.error(`\nFAIL: ${failed} modules do not parse (allowed ${max})`);
  process.exit(1);
}
console.log(`\nOK: within the allowed ${max}`);
