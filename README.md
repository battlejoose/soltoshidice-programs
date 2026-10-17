# SoltoshiDICE program source verification

Public contract source and reproducible build instructions for the existing
Solana mainnet programs behind [SoltoshiDICE](https://soltoshidice.wtf).
This is a release-source export, not the application development repository.
It contains no deployment keys, private RPC credentials or application history.

## Status — October 17, 2026

| Program | Scope | Local Docker reproduction | Public registry |
| --- | --- | --- | --- |
| `CuTLp7pDmNGkFgi4aoh8Ef1YSjc2BzECQRLzYqaoVWBR` | Bank, Cee-lo, slots, blackjack, staking, profiles, referrals | Build-environment investigation in progress; not yet a match | Not registered |
| `3EB6JJ2k1yPdg9qokViBdSWq4jaEVG4wiKbS8Pw3FmWQ` | Hold'em and tournaments | Exact deployed bytes reproduced | Registration pending |
| `5PrHPCCbKpW4SvBZfXmt23RKMaShSM4XGTJdBh8qpw3q` | Street Fights | Exact deployed bytes reproduced | Registration pending |
| `96hGHn4JD3AyzXvNPdx4RyNtS8y7sQ5NizagMsdMAXhG` | Rock Paper Scissors | Exact deployed bytes reproduced | Registration pending |
| `7YWUnPaP5tQjJTC5kZu235P5Qix2VpoJk5rTMgkXeCw7` | SDICE Perps | Exact deployed bytes reproduced | Registration pending |

No deployed program, authority, custody account or game state was changed to
produce this evidence. Verification metadata belongs to a separate verification
program. Local reproduction is distinct from remote verifier acceptance.

## Reproduce independently

Use Linux x86-64, Docker, and `solana-verify` 0.5.2 from the
[Solana Foundation release](https://github.com/solana-foundation/solana-verifiable-build/releases/tag/v0.5.2).
Verify the release binary against its published SHA-256 file.

```bash
bash verify.sh poker
bash verify.sh fights
bash verify.sh rps
bash verify.sh perps
# Currently expected to fail the bytecode comparison; investigation is ongoing:
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

## Source provenance and build details

`source-provenance.json` identifies the original source revisions and file hashes.
`programs.json` identifies program addresses, paths and expected executable hashes.
`evidence/mainnet-snapshot.json` records finalized bytecode observations, deployment
slots and upgrade authorities. None of these files contains private signing keys.

- Bank source is the recorded slots release tree, including its separate pure
  slots-rules crate. Its exact reproducible build remains under investigation.
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
