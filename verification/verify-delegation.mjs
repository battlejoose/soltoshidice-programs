#!/usr/bin/env node
// Read-only. No wallet, credentials or transactions. Keep alongside the downloaded kit.
import { readFile } from 'node:fs/promises';
import { verifyDelegationEvidence, loadDelegationEvidence } from './platform-verifier.mjs';
const args = process.argv.slice(2);
const value = key => args.includes(key) ? args[args.indexOf(key) + 1] : undefined;
if (args.includes('--help')) {
  console.log('node verify-delegation.mjs [--rpc HTTPS_RPC] [--archived]\nDefaults to fresh finalized mainnet reads. Exit 0: delegation checks pass; 1: not verified. Does not verify trading or payouts.');
} else {
  try {
    const archived = args.includes('--archived');
    const evidence = archived ? JSON.parse(await readFile(new URL('./games/perps-delegation.json', import.meta.url), 'utf8'))
      : await loadDelegationEvidence(value('--rpc'));
    console.log(JSON.stringify({...await verifyDelegationEvidence(evidence), evidenceMode: archived ? 'archived' : 'live-finalized-rpc'}, null, 2));
  } catch (e) {
    console.log(JSON.stringify({status: 'not-verified', scope: 'perps-mainnet-delegation-only', error: e.message, transactionsSubmitted: 0}));
    process.exitCode = 1;
  }
}
