# SoltoshiDICE program source verification

## October 25, 2026 — poker tournament seat lock

The live poker program now refuses voluntary tournament withdrawals and idle
kicks. Chips remain seated until elimination or terminal settlement. Four tables,
40-player capacity, account layouts and cash poker are unchanged. All 186 native
tests passed, including upgrading a live hand with an old deferred exit request.
The pinned Docker rebuild matches the finalized executable byte for byte.
See [the release receipt](evidence/poker-seat-lock-release-2026-10-25.json).
Independent registry refresh is pending; `programs.json` tracks its current state.

October 22 poker update: [finalized AFK board-unlock recovery release](evidence/poker-afk-release-2026-10-22.json).
The local and independent remote Docker builds match the deployed executable.
The public registry verifies source `ad183cf73079b61ca5170f68e78227ee6d0da08e`. Four tables / 40 players,
account layouts, chips and operator authority are unchanged. The current poker
source is in `games/poker`; run `bash verify.sh poker` to reproduce its build.

October 21 addition: [separate RPS/fighting bracket program source and finalized mainnet receipts](brackets/README.md).
Poker stays at four tables / 40 players; none of the existing five programs was
upgraded by this release. The bracket program now has independent reproducible-build
verification: [exact source, matching hashes and finalized registration](brackets/verification.json).
Run `bash verify.sh brackets` to repeat it. This October 21 snapshot verified six program executables;
IDL/security records below cover the original five programs.

Public contract source and reproducible build instructions for the existing
Solana mainnet programs behind [SoltoshiDICE](https://soltoshidice.wtf).
This is a release-source export, not the application development repository.
It contains no deployment keys, private RPC credentials or application history.

[Back to the illustrated project overview](README.md). This guide preserves the release records and reproducible-build instructions; dated snapshots describe their original observations.

## Bank is permanently immutable — October 20, 2026 (America/Denver)

The upgrade authority of bank program `CuTLp7pDmNGkFgi4aoh8Ef1YSjc2BzECQRLzYqaoVWBR`
was permanently revoked at finalized slot **459508498** (October 21, 05:14 UTC).
Its ProgramData `9HF5BoewAreFTHQgR6K3mvgkcG5cLy1mF4mnsUkHWBVM` now records
**no upgrade authority**. The complete executable hash is unchanged:
`9c382bb7d94029c097ee2edc37ad6b142fde2aa2b3695887553fa3f8def77912`.
Verified source remains `8c86ff6a501d385508fbe4953f7aa515f2054cb7`.

See the [finalized before/after receipt](evidence/bank-upgrade-authority-revocation-2026-10-21.json)
and [Solscan transaction](https://solscan.io/tx/35NRam9o7P2pHkfgg4yfHo3cnuY7o1WPxqxuLrpEadGxmn1FG7NfLiYtLGPLxy8WWe4ZAQodMfwCajYSeYNiYHEM).
An independent read through Solana's public mainnet RPC at slot `459508662`
confirmed authority option `0` and the unchanged bytecode hash.

**Only the bank's code was finalized.** Poker, Fights, RPS and Perps retain
their upgrade authorities and unchanged bytecode. The bank's existing
administrative controls and external dependencies are unchanged. Historical
release snapshots below describe their original observation dates; this notice
supersedes their bank-upgrade-authority status. Explorer caches may refresh later.

## Read-only game verification kit

[Read the public verifier source and evidence](verification/README.txt), then run
`node verification/verify-platform.mjs --all --live` with Node 22+. No install,
wallet, wager or private repository access is needed. These readable JavaScript
modules are the code distributed by the site's kit; compare each file against
[the SHA-256 source manifest](verification/source-manifest.json).

Five games have scoped proof/outcome/payment checks. Poker remains partial and
Perps is not tested by this kit; exit code 2 preserves those limits. Offchain
Workers provide coordination/transport; native program enforcement and the
client/rollup exceptions are described in [the coverage index](verification/games/index.json).

## Public IDLs and security reporting

All five programs have canonical on-chain IDLs and security records. See
[metadata coverage, Solscan links and finalized receipts](metadata/README.md).
Native IDLs describe decoding; they are not generic transaction-building SDKs.
Report vulnerabilities privately through our [security policy](SECURITY.md).

## Perps mixed backing and native markets — October 23, 2026 UTC

Bet backing can now combine a market’s primary token, SOL and USDC. Each bet
still spends one selected token. Separate native SOL and USDC market creation
is supported, with non-transferable pSOL/pUSDC and profits paid in the market’s
primary token. Native markets allocate their special loss share to main pSDICE
stakers: 2% below the high-pool tier, or 10% to those stakers plus 9% SDICE burn
above $2M. The FIFO allocation remains 99% pool / 1% gas.

The deployed ELF is `ca6e22ecaee311b067fc8ebe088b332c3a4c1fbcce535901c3aa2a4de849dd24`.
It adds append-only wallet records; the migration preserves existing stable open
positions, balances and stored liquidation prices. Older slot-compaction layouts
still require idle wallets. Perps remains upgradeable; no other program or
authority is changed. The source is exported from application commit
`f13964d2566acafabdfe58758a0dc3c87ebcf227`; the public source rebuilt byte for byte to the installed executable. See the
[deployment and rebuild receipt](evidence/perps-mixed-backing-release-2026-10-23.json),
current source in `perps/`, and interface in `metadata/perps.idl.json`. The
independent public registry also verified the same source and mainnet hash on
October 23, 2026: [verification status](https://verify.osec.io/status/7YWUnPaP5tQjJTC5kZu235P5Qix2VpoJk5rTMgkXeCw7).

## Perps spot-token profit payouts — October 21, 2026 UTC

Winning profits now pay directly in each market's backing token at the USD spot
price when paid, without a swap to USDC. Existing unpaid FIFO claims follow the
same rule; historical USDC payments remain recorded separately. Pool reward
and risk-limit valuation also uses spot. Collateral backing and existing
position liquidation thresholds are preserved. The bank is unchanged.

The new redemption instruction only runs at the top level and requires its
pricing accounts to be read-only for the entire transaction. This blocks atomic
reserve manipulation followed by redemption, including a flash-loan transaction.
It does not prevent price manipulation across separate transactions or volatility.

The published source rebuilt to the deployed SHA-256
`07578b1054b56d1dd34c2e2d1ee47405fcb2fec5f97e6aee8675d6be4fdc4966`.
See [source/build evidence](evidence/perps-spot-payout-source-2026-10-21.json).
The canonical IDL is finalized, and independent remote verification passed
for source `8821a2a604750eafdc27742893f73a5598edad04`. See the
[final release and verification receipt](evidence/perps-spot-payout-release-2026-10-21.json).
Earlier records below are historical snapshots. Perps remains upgradeable.

## Perps position rules — October 21, 2026 UTC

New Perps positions liquidate at a full `1 / leverage` adverse move (0.1% at 1000×) and require five seconds before a voluntary close. Liquidations and forced backing exits remain immediate. Existing positions retain their stored liquidation thresholds; account and instruction layouts are unchanged.

The deployed program at slot **459487920** retains its upgrade authority. Public source `dba12051b0ca9c735dd94409d215211a1b903ec9` independently matches its executable; canonical IDL now includes error 6022 MinimumHold. See [final verification and IDL receipts](evidence/perps-position-rules-verification-2026-10-21.json) and [chain snapshot](evidence/perps-position-rules-snapshot-2026-10-21.json). Other programs are unchanged.

## Bank stake contributions — October 21, 2026 UTC

The bank now supports owner-signed partial or full stake contributions into the
main SDICE Perps payout pool, including locked and Forever stakes. Existing
withdrawal restrictions remain unchanged. Credited pSDICE is non-transferable.
The bank upgrade at slot **459463816** initially retained its upgrade authority;
it was subsequently revoked as recorded above.
Public source `8c86ff6a501d385508fbe4953f7aa515f2054cb7` independently rebuilds to
the deployed executable; the registry reports verified. The canonical bank IDL
now includes tag 116 (99 instruction variants, 31 account layouts).
See [verification evidence](evidence/bank-contribution-verification-2026-10-21.json)
and [finalized IDL publication receipts](evidence/bank-contribution-metadata-2026-10-21.json).
The Perps program itself is unchanged by this bank release.

## Bank competition upgrade — October 19, 2026

The bank was upgraded at mainnet slot **458975234** to add separate blackjack
and slots competitions. All three game competitions have a 100,000 SDICE minimum,
1–24-hour selectable duration and a maximum 24-hour scheduled-start horizon.
That release retained upgrade authority; the subsequent bank-only revocation is recorded above.

The exported source reproduces the complete 844,256-byte deployed executable:
`32faf85840d3ce1ee288afc4968e9ed4007600b3a854dce8cdcefae549f25d92`.
The pinned Docker build matches exactly. The independent verifier accepted public
commit `c65b739e5a46f2bd3ba12058bec2d40bb9d3aaf5` on October 19, 2026.
The metadata registration transaction was
`wVBRpu83adBKNqHJv1v74NNdNa89EVeEGE2ttr3KKtJXJf9FC3aUFtbwFN4hntUacHNLLQPKT99xNZ9EvC9QMC8`.
See [the bank upgrade snapshot](evidence/bank-competitions-snapshot-2026-10-19.json).
Other program sources and deployed programs are unchanged by this release.

## Perps tokenomics upgrade — October 20, 2026

Perps was upgraded at mainnet slot **459424476**. This release adds full-loss
rewards with a pool-value taper, doubled FIFO loss rewards, root-pool contributions,
and continuous per-position and combined per-wallet/venue exposure limits.
Existing position layouts and upgrade authority are unchanged.

The exported source rebuilds to the complete 2,030,840-byte mainnet executable:
`a5e884d7015a62d428ef165e15626dfebd4ceb9f4db1562f082fb93bf3949eaa`.
The executable served by MagicBlock matches too. The native pinned-toolchain
rebuild passed, and the independent Docker verifier accepted public source commit
`f82f527a44a15292ededb544d9129fc6a5459804` with matching on-chain and executable
hashes. The canonical on-chain IDL and security record were updated and read back.
See [the current chain snapshot](evidence/perps-tokenomics-snapshot-2026-10-20.json)
and [source provenance](evidence/perps-tokenomics-source-2026-10-20.json).
Final verification and metadata receipts are in
[the October 20 evidence](evidence/perps-tokenomics-verification-2026-10-20.json).

## Status — October 20, 2026

| Program | Scope | Local Docker reproduction | Public registry |
| --- | --- | --- | --- |
| `CuTLp7pDmNGkFgi4aoh8Ef1YSjc2BzECQRLzYqaoVWBR` | Bank, Cee-lo, slots, blackjack, competitions, staking, profiles, referrals | Exact deployed bytes reproduced | [Verified](https://verify.osec.io/status/CuTLp7pDmNGkFgi4aoh8Ef1YSjc2BzECQRLzYqaoVWBR) |
| `3EB6JJ2k1yPdg9qokViBdSWq4jaEVG4wiKbS8Pw3FmWQ` | Hold'em and tournaments | Exact deployed bytes reproduced | [Verified](https://verify.osec.io/status/3EB6JJ2k1yPdg9qokViBdSWq4jaEVG4wiKbS8Pw3FmWQ) |
| `5PrHPCCbKpW4SvBZfXmt23RKMaShSM4XGTJdBh8qpw3q` | Street Fights | Exact deployed bytes reproduced | [Verified](https://verify.osec.io/status/5PrHPCCbKpW4SvBZfXmt23RKMaShSM4XGTJdBh8qpw3q) |
| `96hGHn4JD3AyzXvNPdx4RyNtS8y7sQ5NizagMsdMAXhG` | Rock Paper Scissors | Exact deployed bytes reproduced | [Verified](https://verify.osec.io/status/96hGHn4JD3AyzXvNPdx4RyNtS8y7sQ5NizagMsdMAXhG) |
| `7YWUnPaP5tQjJTC5kZu235P5Qix2VpoJk5rTMgkXeCw7` | SDICE Perps, pool tokenomics upgrade | Native rebuild matches; independent Docker verification passed | [Verified](https://verify.osec.io/status/7YWUnPaP5tQjJTC5kZu235P5Qix2VpoJk5rTMgkXeCw7) |

The original source-verification work changed no deployed program, authority,
custody account or game state. The later bank authority revocation is recorded
above. Verification metadata belongs to a separate verification
program. Local reproduction is distinct from remote verifier acceptance.

The following is historical October 18 evidence, superseded for Perps by the
October 20 upgrade above. The October 17 verification covered all five programs. Perps was subsequently
upgraded at mainnet slot **458267056**; the old Perps verification no longer
matches. This export contains the upgraded source from release commit
`4bdf1057c95ee5e701b41452c4aa356ae60ad506`. The current full executable SHA-256 is
`f539733115c02fc778b9c081b5e4fb12ddd0b989f3089343dfbb23f7f53170da`
(1,935,848 bytes); its zero-trimmed verifier hash is
`c3e105266b4a9a2d74487bf799c775c88b78fd7bfad615aa3602ec2ff6ac3b94`.
The clean pinned Docker build matches every deployed byte.
The verification metadata points to public commit
`a67357f6fe18755b8a33e337bcb8350dc5fd7020`. The independent verifier accepted
that commit on October 18, 2026, with matching on-chain and executable hashes.
The metadata update cost 0.000005 SOL and did not modify the deployed program
or interrupt trading. Other program sources are unchanged. See
[the current Perps chain snapshot](evidence/perps-upgrade-snapshot-2026-10-18.json).

Historical finalized registration
transactions, exact verified commits, costs and unchanged deployment checks are
in [`evidence/verification-receipts.json`](evidence/verification-receipts.json).
The historical bank verification uses commit `e875a74a72af04b0bb203ac2952401e2eb0cef0c`; the other four historical jobs verified `d2c0b0b7fd14b727d925b137f64ee910261c9f3c`. Later documentation-only commits
do not replace the precise source commit recorded by the verifier.

Historical upgrade evidence: [`evidence/perps-upgrade-verification-2026-10-18.json`](evidence/perps-upgrade-verification-2026-10-18.json).

## Reproduce independently

Use Linux x86-64, Docker, and `solana-verify` 0.5.2 from the
[Solana Foundation release](https://github.com/solana-foundation/solana-verifiable-build/releases/tag/v0.5.2).
Verify the release binary against its published SHA-256 file.

```bash
bash verify.sh poker
bash verify.sh fights
bash verify.sh rps
bash verify.sh perps
bash verify.sh bank
```

These commands build and read mainnet. They never read a wallet, send a
transaction or deploy a program. Set `VERIFICATION_RPC_URL` privately if the
public mainnet RPC rate limits you. Every build uses the pinned official image
in `programs.json`: Solana CLI 3.1.10 / SBF platform-tools v1.52 / Rust 1.89.0.

`solana-verify` hashes strip trailing zero padding before comparison. The
recorded full-file SHA-256 and binary length are also supplied. Poker's allocated
program account is larger than its executable; all remaining bytes are zero.
Compare current chain state: upgrades after this snapshot can invalidate it.

## Perps upgrade source

The October 18 Perps export includes modular token venues, the shared ETH-based
winnings haircut curve, queue-first loss allocation, 1% SOL fee funding, 2%
total burns (split between the venue token and SDICE for non-root venues), and
admin fee-surplus withdrawal with a protected reserve minimum. The activated
economics policy has no deadband. This is contract source only; application
servers, credentials and private state are excluded.

## Source provenance and build details

`source-provenance.json` identifies the original source revisions and file hashes.
`programs.json` identifies program addresses, paths and expected executable hashes.
`evidence/mainnet-snapshot.json` records finalized bytecode observations, deployment
slots and upgrade authorities. None of these files contains private signing keys.

- Bank source is the recorded slots release tree, including its separate pure
  slots-rules crate. The original Cargo crate identifiers depend on the original
  filesystem path. A transparent Rust compiler wrapper pins only `-C metadata`
  for `hooddice_bank` and `sdice_slots_rules` to the original values. It does not
  change compiler code, executable statements, or compiled bytes.
- The bank export removes nine comment-only lines from `src/slots/mod.rs` to
  reconstruct embedded panic locations (407, 408, 497 rather than 416, 417, 506).
  This restores the released binary without changing any executable source
  statement. The original file hash and this transformation are recorded in
  `source-provenance.json`. Other comments remain unchanged.
- Fights includes its original September 13 ORAO adapter. The newer bank adapter
  was never deployed into the fight program and must not replace it here.
- Poker's mainnet `declare_id!` substitution is the same one used by its original
  release procedure; the original private source defaulted to a devnet identity.
- Perps retains its original build manifest and production policy byte-for-byte.
  Historical names and pre-release status notes in that compile input are not
  live deployment status. Its `.cargo/config.toml` records the original mainnet
  feature environment and size-optimization setting. The workspace is narrowed
  to this program; original dependency versions and release profile are retained.

This export is for reproducing the onchain executables. Some historical test
harnesses reference fixtures/tools outside this export; no claim is made that
every original application test can run from this repository alone.

## What verification establishes

A matching build establishes that the published source compiles to the deployed
executable. It is not an independent security audit, a guarantee against losses,
or proof of the correctness of an oracle or offchain component. Upgrade authority
remains relevant. Poker's client-side shuffle proofs and Perps' rollup/oracle
services have additional trust boundaries described in the
[public protocol documentation](https://soltoshidice.wtf/agents).

See [Solana Verified Builds](https://solana.com/docs/programs/verified-builds).
Live registry status: `https://verify.osec.io/status/PROGRAM_ADDRESS`.

Security contact: battlejoose@gmail.com. Public visibility is provided for review
and reproduction; it does not by itself grant a new software license.
