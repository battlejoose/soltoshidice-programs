SoltoshiDICE read-only mainnet evidence checks. Node 22+.
Inspect the code, then: node verify-platform.mjs --all --live
Without --live: archived evidence only. Use --game slots for one game.
Exit 0: covered checks pass; 1: not verified; 2: partial/untested.
Cee-lo, fights, RPS, slots and blackjack: scoped proof/outcome/payment replays.
Poker: revealed cards/chip awards only; no full shuffle transcript or wallet cash-out.
Blackjack: draw proofs and table escrow returns; not wallet cash-out.
Perps: not tested by this kit. No new wagers; no transactions submitted.
Public source: https://github.com/battlejoose/soltoshidice-programs/tree/main/verification
The .mjs files are readable executable source, not minified binaries.
Compare every listed file against source-manifest.json; the public repository and website distribute the same kit files.
Direct source: https://soltoshidice.wtf/verification/platform-verifier.mjs
Direct runner: https://soltoshidice.wtf/verification/verify-platform.mjs
Dice verifier: https://soltoshidice.wtf/verification/verify-fairness.mjs
No access to the private application repository is needed.
Offchain Workers coordinate sessions/views and submit transactions; they are not Solana programs. Inspect games/index.json execution roles and native program receipts separately.
See games/index.json and each result for exact limitations.
