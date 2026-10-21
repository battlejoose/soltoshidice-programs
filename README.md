# SoltoshiDICE program source verification

Public contract source and reproducible build instructions for the existing
Solana mainnet programs behind [SoltoshiDICE](https://soltoshidice.wtf).
This is a release-source export, not the application development repository.
It contains no deployment keys, private RPC credentials or application history.

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

## Bank stake contributions — October 21, 2026 UTC

The bank now supports owner-signed partial or full stake contributions into the
main SDICE Perps payout pool, including locked and Forever stakes. Existing
withdrawal restrictions remain unchanged. Credited pSDICE is non-transferable.
The bank upgrade at slot **459463816** retains its upgrade authority.
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
Upgrade authority remains unchanged. No authority revocation is claimed.

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

No deployed program, authority, custody account or game state was changed to
produce this evidence. Verification metadata belongs to a separate verification
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
