#!/usr/bin/env node
// Read-only Node 22+ runner. Keep this file, both verifier modules and games/ together.
import { readFile } from 'node:fs/promises';
import { verifyGameEvidence, refreshGameEvidence, summarizeVerifiedPayouts } from './platform-verifier.mjs';
import { verifyEvidence, loadLiveEvidence } from './verify-fairness.mjs';
const args = process.argv.slice(2), option = key => args.includes(key) ? args[args.indexOf(key) + 1] : undefined;
const read = async path => JSON.parse(await readFile(new URL(path, import.meta.url), 'utf8'));
const manifest = await read('./games/index.json');
const live = args.includes('--live'), rpc = option('--rpc') || 'https://api.mainnet-beta.solana.com';
const game = option('--game'), file = option('--file');
if (args.includes('--help') || (!game && !file && !args.includes('--all'))) {
  console.log('node verify-platform.mjs --all [--live] [--rpc HTTPS_RPC]\nnode verify-platform.mjs --game cee-lo|fights|rps|slots|blackjack|poker [--live]\nnode verify-platform.mjs --file ./games/slots.json [--live]\nExit: 0 covered checks passed; 1 not verified; 2 partial/untested. No wallet or transactions.');
  process.exit(args.includes('--help') ? 0 : 1);
}
const results = [];
try {
  const supplied = file ? JSON.parse(await readFile(file, 'utf8')) : null;
  const selected = supplied ? [{ id: supplied.game, supplied }] : manifest.games.filter(g => !game || game === g.id);
  if (!selected.length) throw Error('Unknown game');
  for (const item of selected) {
    try {
      const dice = item.id === 'cee-lo';
      let b = item.supplied || await read(dice ? './fairness-example.json' : './games/' + item.id + '.json');
      if (live) b = dice ? await loadLiveEvidence(b, rpc) : await refreshGameEvidence(b, rpc);
      const r = dice ? await verifyEvidence(b) : await verifyGameEvidence(b);
      results.push({ ...r, game: item.id, evidenceMode: live ? 'live-finalized-rpc' : 'archived' });
    } catch (e) { results.push({ game: item.id, status: 'not-verified', error: e.message, evidenceMode: live ? 'live-finalized-rpc' : 'archived' }); }
  }
  if (args.includes('--all')) results.push(...manifest.otherProducts);
} catch (e) { results.push({ status: 'not-verified', error: e.message }); }
const failed = results.some(r => r.status === 'not-verified'), partial = results.some(r => ['partial', 'not-tested'].includes(r.status));
console.log(JSON.stringify({ status: failed ? 'not-verified' : partial ? 'partial' : 'pass', checkedAt: new Date().toISOString(), transactionsSubmitted: 0, payoutSummary: summarizeVerifiedPayouts(results), results }, null, 2));
process.exitCode = failed ? 1 : partial ? 2 : 0;
