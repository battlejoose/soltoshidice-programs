# Public IDLs and security contacts

These records describe the five mainnet programs in [`programs.json`](../programs.json).
They are published in canonical accounts owned by Solana's
[Program Metadata Program](https://github.com/solana-program/program-metadata),
using the `idl` and `security` seeds. Publication is signed by each program's
existing upgrade authority. It does not upgrade executable code or alter game
accounts, balances held by the games, or program upgrade authorities.

## Perps update — October 20, 2026

The new Perps IDL contains 85 instructions and 23 account layouts, including
pool exposure and direct contributions. Its security record points to verified
public source commit `f82f527a44a15292ededb544d9129fc6a5459804`. The replacement
was uploaded to a temporary buffer, then installed atomically; the buffer rent
was returned. All ten canonical records were read back successfully. See
[finalized Perps verification and metadata receipts](../evidence/perps-tokenomics-verification-2026-10-20.json).

## Bank update — October 19, 2026

The bank IDL now includes separate game competitions and selectable duration.
Its security record points to the newly verified source. All ten public records
match their canonical accounts: [finalized update receipts](../evidence/bank-metadata-update-2026-10-19.json)
and [public API readback](../evidence/metadata-api-2026-10-19.json).
Additional storage and fees for this update totaled 0.002680395 SOL.

## Original publication — October 18, 2026

All five IDLs and five security records are finalized. All five Solscan program
pages render their public IDLs and show `Security.txt: TRUE`. The public Solana
IDL Explorer API returns all ten records, matching the JSON files here.

| Program | Solscan public IDL |
| --- | --- |
| Bank / dice / blackjack / slots / staking | [View](https://solscan.io/account/CuTLp7pDmNGkFgi4aoh8Ef1YSjc2BzECQRLzYqaoVWBR#programIdl) |
| Poker | [View](https://solscan.io/account/3EB6JJ2k1yPdg9qokViBdSWq4jaEVG4wiKbS8Pw3FmWQ#programIdl) |
| Block Fights | [View](https://solscan.io/account/5PrHPCCbKpW4SvBZfXmt23RKMaShSM4XGTJdBh8qpw3q#programIdl) |
| Rock Paper Scissors | [View](https://solscan.io/account/96hGHn4JD3AyzXvNPdx4RyNtS8y7sQ5NizagMsdMAXhG#programIdl) |
| Perps | [View](https://solscan.io/account/7YWUnPaP5tQjJTC5kZu235P5Qix2VpoJk5rTMgkXeCw7#programIdl) |

The complete [finalized receipts](../evidence/program-metadata-receipts-2026-10-18.json)
show unchanged executable hashes, deployment slots and upgrade authorities.
Storage holds **0.257053080 SOL**; actual transaction fees were **0.000398667 SOL**,
for **0.257451747 SOL** total. This includes replacing three native IDL records
to retain empty arrays required by Solscan's renderer. Their original storage
rent was recovered to the same authority and reused; no game funds were touched.

[Public API readback](../evidence/program-metadata-api-2026-10-18.json) and
[Solscan observations](../evidence/program-metadata-solscan-2026-10-18.json)
are recorded separately from transaction finality.

## Coverage

| Program | IDL format | Instruction payloads | Account layouts |
| --- | --- | ---: | ---: |
| Bank, Cee-lo, blackjack, slots, staking | Codama | 98 | 31 |
| Poker | Anchor | 42 | 13 |
| Block Fights | Codama | 8 | 3 |
| Rock Paper Scissors | Codama | 8 | 3 |
| Perps | Anchor | 85 | 23 |

The Anchor IDLs come from the exact deployed releases. Their instruction and
account discriminators were checked. See [Anchor provenance](anchor-provenance.json).

The three native programs use one-byte instruction tags, fixed magic prefixes,
and Borsh account layouts rather than Anchor's eight-byte discriminators. Their
Codama IDLs are **decode-only**: instruction account ordering and permissions are
not supplied, and variable optional instruction tails are retained as bytes.
Use the maintained client builders and verified Rust source to construct
transactions. Some native account layouts share a size and version; verify the
owning program and PDA before choosing a layout. These files are not a generic
transaction-building SDK. See [native provenance](native-provenance.json).

Validation covered 13 native instruction fixtures and 430 fields from eight live
accounts against the maintained client decoders. No gameplay transaction was
submitted for validation. This is ABI validation, not a security audit.

## Read the canonical records

```sh
npx @solana-program/program-metadata@0.11.0 fetch idl <PROGRAM_ID>
npx @solana-program/program-metadata@0.11.0 fetch security <PROGRAM_ID>
```

The [Solana IDL Explorer](https://idl.solana.com/) also resolves canonical
records. Solscan's public IDL importer uses this registry; explorer caches and
support for individual IDL formats may differ. A private Solscan Custom IDL is
not the same as public canonical publication.

Security records link to our [security policy](../SECURITY.md) and
[private vulnerability reporting](https://github.com/battlejoose/soltoshidice-programs/security/advisories/new).
They are stored separately from the executable. No ELF security section was
inserted. Readers that only inspect embedded ELF sections must add PMP support
to discover these records; Solscan and the Solana IDL Explorer resolved them in
the checks above.

The native JSON retains empty arrays for compatibility with Solscan's Codama
renderer. Removing these arrays with a compact serializer can break that view
even though the JSON remains valid Codama.

Future executable upgrades must be checked against these IDLs. Publish an
updated IDL when the interface changes; retain the precise source revision and
transaction evidence for each publication.
