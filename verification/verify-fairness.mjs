// Read-only Cee-lo v4 evidence verifier. Node 22+ or a browser with WebCrypto Ed25519.
// No dependencies, wallet, secrets, transaction submission, or project RPC proxy.
// node verify-fairness.mjs --live [--rpc https://your-trusted-rpc]
// node verify-fairness.mjs --file fairness-example.json   (archived evidence only)
export const IDS = Object.freeze({
  program: 'CuTLp7pDmNGkFgi4aoh8Ef1YSjc2BzECQRLzYqaoVWBR',
  bank: '9vBaKrxgtQEc6zavSVTvptAM2oRmX38SkyHXpfvUFoKR',
  vault: '951nS8NJoi7ajueDo7iztaWQiapUZuHAx5xt3vVApJXa',
  mint: '4nCmpwne7hCoWTSpAd54uENmCgHJrHTyn4DMPCEMpump',
  token: 'TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb',
  oracle: 'VRFzZoJdhFWL8rkvu87LpKM3RbcVezpMEc6X5GVDr7y',
  network: '5ER1oENnV4srxYdAynUfRzWeQCPQaqMiAp4VqyMbSqnK',
  ed25519: 'Ed25519SigVerify111111111111111111111111111',
  genesis: '5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d',
});
const BASE = 'https://soltoshidice.wtf/verification/';
const alphabet = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';
const utf8 = s => new TextEncoder().encode(s);
export const base64 = s => Uint8Array.from(atob(s), c => c.charCodeAt(0));
export function decode58(s) {
  need(typeof s === 'string' && s.length > 0, 'Missing base58 value');
  let n = 0n;
  for (const c of s) { const v = alphabet.indexOf(c); need(v >= 0, 'Invalid base58'); n = n * 58n + BigInt(v); }
  const bytes = []; while (n) { bytes.unshift(Number(n & 255n)); n >>= 8n; }
  return new Uint8Array([...Array(s.match(/^1*/)[0].length).fill(0), ...bytes]);
}
export function encode58(bytes) {
  let n = 0n, s = '', zeros = 0;
  for (const b of bytes) n = n * 256n + BigInt(b);
  while (n) { s = alphabet[Number(n % 58n)] + s; n /= 58n; }
  for (const b of bytes) { if (b) break; zeros++; }
  return '1'.repeat(zeros) + s;
}
function need(ok, message) { if (!ok) throw Error(message); }
const same = (a, b) => a.length === b.length && a.every((v, i) => v === b[i]);
function equal(a, b, name) { need(same(a, b), name); }
const cat = (...parts) => new Uint8Array(parts.flatMap(p => [...p]));
const hash = async bytes => new Uint8Array(await crypto.subtle.digest('SHA-256', bytes));
const discriminator = async name => (await hash(utf8(name))).slice(0, 8);
const view = b => new DataView(b.buffer, b.byteOffset, b.byteLength);
const u64 = (b, o) => view(b).getBigUint64(o, true);
const i64 = (b, o) => view(b).getBigInt64(o, true);
const keyAt = (b, o) => encode58(b.slice(o, o + 32));
const ixes = tx => tx.transaction.message.instructions;
const keyAtIndex = (tx, i) => tx.transaction.message.accountKeys[i]?.pubkey;
const tokenAmount = (tx, when, address, owner) => {
  const records = tx.meta[when + 'TokenBalances'].filter(b => keyAtIndex(tx, b.accountIndex) === address);
  need(records.length === 1, `Missing ${when} token balance`);
  const b = records[0];
  need(b.owner === owner && b.mint === IDS.mint && b.programId === IDS.token && b.uiTokenAmount.decimals === 6, 'Token balance identity');
  return BigInt(b.uiTokenAmount.amount);
};
function native(tx, tag) {
  const found = ixes(tx).map((ix, index) => ({ ix, index })).filter(({ ix }) => ix.programId === IDS.program && ix.data && decode58(ix.data)[0] === tag);
  need(found.length === 1, `One direct bank instruction ${tag} required`);
  return found[0];
}
// Only accept actual program-owned logs, not an identically named event from another invocation.
function logs(tx, program, depth) {
  const stack = [], result = [];
  for (const line of tx.meta.logMessages ?? []) {
    const start = /^Program (\w+) invoke \[(\d+)\]$/.exec(line);
    if (start) { need(+start[2] === stack.length + 1, 'Invalid log depth'); stack.push(start[1]); continue; }
    const end = /^Program (\w+) (?:success|failed:.*)$/.exec(line);
    if (end) { need(stack.pop() === end[1], 'Invalid log owner'); continue; }
    if (stack.length === depth && stack.at(-1) === program && line.startsWith('Program data: ')) result.push(line.slice(14).split(' ').map(base64));
  }
  need(stack.length === 0, 'Incomplete transaction logs');
  return result;
}
async function events(tx, name, depth) {
  const d = await discriminator('event:' + name);
  return logs(tx, IDS.oracle, depth).filter(p => p.length === 1 && same(p[0].slice(0, 8), d)).map(p => p[0]);
}
async function pdaWithBump(seeds, bump, expected) {
  // Seed/bump address match. Canonical off-curve selection is enforced by the chain program.
  equal(await hash(cat(...seeds, [bump], decode58(IDS.program), utf8('ProgramDerivedAddress'))), decode58(expected), 'PDA seeds/bump do not match');
}
function score(dice) {
  if (dice.some(d => d < 1 || d > 6)) return 0;
  const [a, b, c] = [...dice].sort();
  return a === c ? 200 + a : a === 4 && b === 5 && c === 6 ? 100 : a === 1 && b === 2 && c === 3 ? 1 : a === b ? 10 + c : b === c ? 10 + a : 0;
}
async function roll(randomness, round, player) {
  for (let attempt = 0; attempt < 32; attempt++) {
    const dice = [];
    for (let block = 0; block < 8 && dice.length < 3; block++) {
      const bytes = await hash(cat(utf8('hooddice:shared:v1'), randomness, decode58(round), player ? decode58(player) : new Uint8Array(32), [player ? 1 : 0, attempt, block]));
      for (const b of bytes) if (b < 252 && dice.length < 3) dice.push(b % 6 + 1);
    }
    if (dice.length < 3) return { dice: [0, 0, 0], throws: attempt + 1 };
    if (score(dice) > 0) return { dice, throws: attempt + 1 };
  }
  return { dice: [0, 0, 0], throws: 32 };
}
function transfer(tx, index, from, to, authority, amount) {
  const matches = tx.meta.innerInstructions.filter(g => g.index === index).flatMap(g => g.instructions).filter(ix => ix.programId === IDS.token && ix.stackHeight === 2 && ix.parsed?.type === 'transferChecked' && ix.parsed.info.source === from && ix.parsed.info.destination === to);
  need(matches.length === 1, 'One matching Token-2022 transfer required');
  const p = matches[0].parsed.info;
  need(p.authority === authority && p.mint === IDS.mint && p.tokenAmount.decimals === 6 && BigInt(p.tokenAmount.amount) === amount, 'Token transfer amount/mint/authority mismatch');
}
export async function verifyEvidence(bundle) {
  need(bundle.schemaVersion === 1 && bundle.genesisHash === IDS.genesis, 'Mainnet genesis/schema mismatch');
  const checks = [], pass = (id, detail) => checks.push({ id, status: 'pass', detail });
  const { wager, request, responses, claim, cleanup } = bundle.transactions;
  need(Array.isArray(responses) && responses.length > 0, 'No ORAO response transactions');
  const records = [wager, request, ...responses, claim, cleanup];
  const signatures = new Set();
  for (const { signature, receipt: tx } of records) {
    need(tx?.meta && tx.transaction && tx.meta.err === null, 'Missing or failed transaction');
    need(tx.transaction.signatures[0] === signature && decode58(signature).length === 64, 'Transaction identity mismatch');
    need(!signatures.has(signature), 'Duplicate transaction'); signatures.add(signature);
    need(Number.isSafeInteger(tx.slot) && tx.slot > 0, 'Missing slot');
  }
  pass('receipts', `${records.length} successful transaction receipts; mainnet genesis matches`);
  const tx = cleanup.receipt, finalIx = native(tx, 52).ix;
  need(finalIx.data === 'u' && finalIx.accounts[3] === IDS.bank, 'Wrong cleanup bank');
  const final = logs(tx, IDS.program, 1).filter(p => same(p[0], utf8('SDICE_FINAL_BET_V1')));
  need(final.length === 1 && final[0].length === 3, 'Missing genuine final-bet receipt');
  const [, r, b] = final[0], round = finalIx.accounts[4], bet = finalIx.accounts[5];
  need(r.length === 272 && b.length === 93 && r[0] === 2 && b[0] === 4, 'Unsupported round/bet ABI');
  need(keyAt(r, 1) === IDS.bank && keyAt(b, 1) === round, 'Round/bank binding');
  const player = keyAt(b, 33), oracle = keyAt(r, 74), seed = r.slice(106, 138), client = keyAt(r, 138), randomness = r.slice(170, 234);
  need(player === finalIx.accounts[9] && oracle === bundle.oracle.address, 'Player/oracle identity mismatch');
  await pdaWithBump([utf8('shared-round'), decode58(IDS.bank), r.slice(33, 41)], r[271], round);
  await pdaWithBump([utf8('bet'), decode58(round), decode58(player)], b[92], bet);
  need(r[41] >= 3 && r[41] <= 5 && b[77] === 1 && b[78] === 1 && r[270] === 0, 'Fulfilled, settled, claimed round required');
  const w = native(wager.receipt, 54), q = native(request.receipt, 42), c = native(claim.receipt, 45);
  need(w.ix.accounts[0] === player && w.ix.accounts[1] === IDS.bank && w.ix.accounts[3] === round && w.ix.accounts[4] === bet, 'Wager is not for this round/player');
  const wagerData = decode58(w.ix.data), stake = u64(b, 65);
  need(wagerData.length === 26 && u64(wagerData, 1) === u64(r, 33) && u64(wagerData, 9) === stake, 'Wager instruction amount/nonce');
  need(q.ix.accounts[3] === IDS.bank && q.ix.accounts[4] === round && q.ix.accounts[8] === oracle, 'Request is not for this round');
  need(c.ix.accounts[3] === IDS.bank && c.ix.accounts[4] === round && c.ix.accounts[5] === bet && c.ix.accounts[21] === player, 'Claim is not for this round/player');
  need(wager.receipt.slot < request.receipt.slot && request.receipt.slot < Math.min(...responses.map(x => x.receipt.slot)) && Math.max(...responses.map(x => x.receipt.slot)) < claim.receipt.slot && claim.receipt.slot < tx.slot, 'Wager/request/fulfillment/payment ordering');
  need(i64(r, 58) >= i64(r, 50), 'Recorded request preceded betting close');
  pass('round-binding', `Round ${u64(r, 33)}: wager locked before request and oracle responses; payment precedes cleanup`);
  const requests = request.receipt.meta.innerInstructions.filter(g => g.index === q.index).flatMap(g => g.instructions).filter(ix => ix.programId === IDS.oracle && ix.stackHeight === 2);
  need(requests.length === 1, 'Missing ORAO request invocation');
  const rq = requests[0], requestBytes = decode58(rq.data);
  equal(requestBytes.slice(0, 8), await discriminator('global:request_v2'), 'Wrong ORAO instruction');
  equal(requestBytes.slice(8), seed, 'Request seed mismatch');
  need(rq.accounts[0] === client && rq.accounts[1] === IDS.network && rq.accounts[3] === oracle, 'Request account binding');
  const requestEvents = await events(request.receipt, 'Request', 2);
  need(requestEvents.length === 1 && requestEvents[0].length === 73, 'Missing ORAO Request event');
  equal(requestEvents[0].slice(8, 40), seed, 'Request event seed');
  need(keyAt(requestEvents[0], 40) === client, 'Request event client');
  const o = bundle.oracle.account.value;
  need(o?.owner === IDS.oracle && o.data?.[1] === 'base64', 'Missing ORAO-owned account');
  const ob = base64(o.data[0]);
  equal(ob.slice(0, 8), [139, 239, 184, 215, 227, 86, 191, 226], 'ORAO account discriminator');
  need(ob.length >= 137 && ob[8] === 1 && keyAt(ob, 9) === client, 'ORAO fulfilled client');
  equal(ob.slice(41, 73), seed, 'Fulfilled seed'); equal(ob.slice(73, 137), randomness, 'Fulfilled output');
  pass('request-binding', 'Bank receipt, ORAO request instruction/event and fulfilled account share seed, client and output');
  const xor = new Uint8Array(64), authorities = new Set(), fulfillments = [];
  for (const { receipt: responseTx } of responses) {
    const ev = await events(responseTx, 'Response', 1);
    need(ev.length === 1 && ev[0].length === 136, 'One authentic ORAO Response event required');
    const response = ev[0], publicKey = response.slice(40, 72), signature = response.slice(72);
    equal(response.slice(8, 40), seed, 'Response seed');
    const authority = encode58(publicKey); need(!authorities.has(authority), 'Duplicate oracle authority'); authorities.add(authority);
    const oracleIx = ixes(responseTx).filter(ix => ix.programId === IDS.oracle);
    need(oracleIx.length === 1 && oracleIx[0].accounts[2] === IDS.network && oracleIx[0].accounts[3] === oracle && oracleIx[0].accounts[4] === client, 'Fulfillment account binding');
    equal(decode58(oracleIx[0].data), await discriminator('global:fulfill_v2'), 'Wrong fulfillment instruction');
    const verificationIx = ixes(responseTx).filter(ix => ix.programId === IDS.ed25519);
    need(verificationIx.length === 1, 'One Ed25519 precompile required');
    need(ixes(responseTx).indexOf(verificationIx[0]) < ixes(responseTx).indexOf(oracleIx[0]), 'Signature must precede fulfillment');
    const ed = decode58(verificationIx[0].data), v = view(ed);
    need(ed.length >= 16 && ed[0] === 1 && ed[1] === 0, 'Unsupported Ed25519 instruction');
    need([4, 8, 14].every(i => v.getUint16(i, true) === 65535), 'Expected self-contained Ed25519 data');
    const sigOffset = v.getUint16(2, true), keyOffset = v.getUint16(6, true), msgOffset = v.getUint16(10, true);
    equal(ed.slice(sigOffset, sigOffset + 64), signature, 'Precompile signature/event mismatch');
    equal(ed.slice(keyOffset, keyOffset + 32), publicKey, 'Precompile authority mismatch');
    need(v.getUint16(12, true) === 32, 'Signed message length'); equal(ed.slice(msgOffset, msgOffset + 32), seed, 'Signed seed mismatch');
    const key = await crypto.subtle.importKey('raw', publicKey, 'Ed25519', false, ['verify']);
    need(await crypto.subtle.verify('Ed25519', key, signature, seed), 'Invalid ORAO Ed25519 signature');
    signature.forEach((byte, i) => { xor[i] ^= byte; });
    fulfillments.push(...await events(responseTx, 'Fulfill', 1));
  }
  need(fulfillments.length === 1 && fulfillments[0].length === 104, 'One final ORAO Fulfill event required');
  equal(fulfillments[0].slice(8, 40), seed, 'Fulfill seed'); equal(fulfillments[0].slice(40), xor, 'Fulfill XOR mismatch');
  equal(xor, randomness, 'Signatures do not produce recorded randomness');
  pass('vrf-signatures', `${authorities.size} distinct Ed25519 signatures verified locally; XOR matches ORAO fulfillment and game randomness`);
  const bankHand = await roll(randomness, round), bankScore = score(bankHand.dice);
  equal(bankHand.dice, r.slice(234, 237), 'Bank dice replay'); need(bankHand.throws === r[237], 'Bank throw count');
  const instant = bankHand.dice.every(n => n === 0) ? 2 : [1, 11].includes(bankScore) ? 1 : [16, 100, 201, 202, 203, 204, 205, 206].includes(bankScore) ? 0 : null;
  const playerHand = instant === null ? await roll(randomness, round, player) : { dice: [0, 0, 0], throws: 0 };
  equal(playerHand.dice, b.slice(79, 82), 'Player dice replay'); need(playerHand.throws === b[82], 'Player throw count');
  const outcome = instant ?? (playerHand.dice.every(n => n === 0) ? 2 : score(playerHand.dice) > bankScore ? 1 : 0);
  const payout = stake * BigInt(outcome === 0 ? 0 : outcome === 2 ? 1 : score(playerHand.dice) >= 100 ? 3 : 2);
  need(b[83] === outcome && u64(b, 84) === payout, 'Outcome/gross payout mismatch');
  need(outcome === 1 && payout > 0n, 'This payment verifier requires a winning round');
  pass('game-replay', `Bank ${bankHand.dice.join('–')}; ${instant === null ? 'player ' + playerHand.dice.join('–') : 'automatic result, no player roll'}; player wins under v4 banker-first rules`);
  const playerAta = w.ix.accounts[7];
  need(w.ix.accounts[5] === IDS.mint && w.ix.accounts[6] === IDS.vault && w.ix.accounts[8] === IDS.token, 'Wager token accounts');
  need(c.ix.accounts[12] === IDS.mint && c.ix.accounts[13] === IDS.vault && c.ix.accounts[14] === playerAta && c.ix.accounts[15] === IDS.token, 'Claim token accounts');
  transfer(wager.receipt, w.index, playerAta, IDS.vault, player, stake);
  need(tokenAmount(wager.receipt, 'post', playerAta, player) - tokenAmount(wager.receipt, 'pre', playerAta, player) === -stake, 'Player stake delta mismatch');
  transfer(claim.receipt, c.index, IDS.vault, playerAta, IDS.bank, payout);
  need(tokenAmount(claim.receipt, 'post', playerAta, player) - tokenAmount(claim.receipt, 'pre', playerAta, player) === payout, 'Player payout delta mismatch');
  need(tokenAmount(claim.receipt, 'pre', IDS.vault, IDS.bank) - tokenAmount(claim.receipt, 'post', IDS.vault, IDS.bank) === payout, 'Bank payout delta mismatch');
  pass('payment', 'Token-2022 stake and winning transfer match; player credit and bank debit equal the calculated payout');
  return { status: 'pass', checkedAt: new Date().toISOString(), cluster: 'mainnet-beta', transactionsSubmitted: 0, checks,
    round, nonce: u64(r, 33).toString(), player, oracle, authorities: [...authorities], bankDice: bankHand.dice, playerDice: playerHand.dice,
    totals: { mint: IDS.mint, decimals: 6, wagerBaseUnits: stake.toString(), grossPayoutBaseUnits: payout.toString(), netPlayerTokenChangeBaseUnits: (payout - stake).toString() },
    receipts: records.map(({ signature, receipt }) => ({ signature, slot: receipt.slot, explorer: 'https://solscan.io/tx/' + signature })),
    limits: [
      'One historical winning Cee-lo round, not all games, all payouts, an RTP audit or a new gameplay test.',
      'Live mode trusts the selected Solana RPC and consensus for finalized inclusion and transaction/log authenticity. Archived mode checks supplied evidence, not fresh chain inclusion.',
      'Oracle signatures and XOR are verified independently. Historical authority-set membership/quorum is trusted to the ORAO program that accepted those responses; historical configuration and executable bytes are not reconstructed here.',
      'PDA seeds and stored bumps are checked; canonical off-curve selection is trusted to the deployed program. Token totals exclude SOL transaction costs and refundable deposits.',
      'Bank code is now immutable; Poker, Fights, RPS and Perps remain upgradeable. Current authority evidence: https://soltoshidice.wtf/verification#bank-immutable. This proof does not remove upgrade-authority, oracle, custody or availability risks.',
    ] };
}
export async function loadLiveEvidence(manifest, rpcUrl = 'https://api.mainnet-beta.solana.com', progress = () => {}) {
  const url = new URL(rpcUrl); need(url.protocol === 'https:' || ['localhost', '127.0.0.1'].includes(url.hostname), 'Use HTTPS for your trusted RPC');
  const rpc = async (method, params = []) => {
    for (let attempt = 0; attempt < 3; attempt++) {
      const res = await fetch(url, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ jsonrpc: '2.0', id: 1, method, params }), signal: AbortSignal.timeout(15000) });
      if (res.status === 429 && attempt < 2) { await new Promise(r => setTimeout(r, 1500 * (attempt + 1))); continue; }
      need(res.ok, `RPC unavailable (HTTP ${res.status}); try a trusted archival RPC`);
      const body = await res.json(); need(!body.error, `RPC unavailable (${body.error?.code}); no verification pass`); return body.result;
    }
  };
  const bundle = structuredClone(manifest); bundle.genesisHash = await rpc('getGenesisHash');
  need(bundle.genesisHash === IDS.genesis, 'RPC is not Solana mainnet');
  const records = [bundle.transactions.wager, bundle.transactions.request, ...bundle.transactions.responses, bundle.transactions.claim, bundle.transactions.cleanup];
  for (let i = 0; i < records.length; i++) {
    progress(`Fetching finalized receipt ${i + 1} of ${records.length}…`);
    records[i].receipt = await rpc('getTransaction', [records[i].signature, { encoding: 'jsonParsed', commitment: 'finalized', maxSupportedTransactionVersion: 0 }]);
    need(records[i].receipt, 'Historical transaction unavailable; use an archival RPC');
    await new Promise(r => setTimeout(r, 400));
  }
  progress('Fetching the ORAO fulfilled account…');
  bundle.oracle.account = await rpc('getAccountInfo', [bundle.oracle.address, { encoding: 'base64', commitment: 'finalized' }]);
  need(bundle.oracle.account?.value, 'ORAO account unavailable; live verification cannot pass');
  return bundle;
}
if (typeof process !== 'undefined' && process.versions?.node && process.argv[1]) {
  const { pathToFileURL } = await import('node:url');
  if (import.meta.url === pathToFileURL(process.argv[1]).href) {
    let mode = 'archived';
    try {
      const args = process.argv.slice(2), option = name => { const i = args.indexOf(name); return i < 0 ? undefined : args[i + 1]; };
      need(args.includes('--live') || option('--file'), 'Use --live or --file <fairness-example.json>');
      const file = option('--file');
      let bundle = file ? JSON.parse(await (await import('node:fs/promises')).readFile(file, 'utf8')) : await (await fetch(BASE + 'fairness-example.json', { signal: AbortSignal.timeout(20000) })).json();
      if (args.includes('--live')) { mode = 'live-finalized-rpc'; bundle = await loadLiveEvidence(bundle, option('--rpc') || process.env.SOLANA_RPC_URL); }
      console.log(JSON.stringify({ ...await verifyEvidence(bundle), evidenceMode: mode }, null, 2));
    } catch (error) { console.log(JSON.stringify({ status: 'not-verified', evidenceMode: mode, error: error.message, transactionsSubmitted: 0 }, null, 2)); process.exitCode = 1; }
  }
}
