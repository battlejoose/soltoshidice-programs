SoltoshiDICE read-only mainnet evidence checks. Node 22+.
Inspect the code, then: node verify-platform.mjs --all --live
Without --live: archived evidence only. Use --game slots for one game.
Exit 0: covered checks pass; 1: not verified; 2: partial/untested.
Cee-lo, fights, RPS, slots and blackjack: scoped proof/outcome/payment replays.
Poker: revealed cards/chip awards only; no full shuffle transcript or wallet cash-out.
Blackjack: draw proofs and table escrow returns; not wallet cash-out.
Perps trading/payouts: not tested. Separate delegation check: node verify-delegation.mjs --rpc https://YOUR_TRUSTED_MAINNET_RPC (fresh finalized reads); --archived for saved evidence. Correct program ends RSaeSh, not Rxhbu. No new wagers; no transactions submitted.
payoutSummary sums only distinct passing examples, with player wallets separate from table escrow. It is not a platform-wide total.
Fresh gameplay and complete payout reconciliation remain separate tests; see /verification#coverage-next.
MagicBlock ER architecture and primary documentation: https://soltoshidice.wtf/verification#ephemeral-rollups
Public source: https://github.com/battlejoose/soltoshidice-programs/tree/main/verification
The .mjs files are readable executable source, not minified binaries.
Compare every listed file against source-manifest.json; the public repository and website distribute the same kit files.
Direct source: https://soltoshidice.wtf/verification/platform-verifier.mjs
Direct runner: https://soltoshidice.wtf/verification/verify-platform.mjs
Dice verifier: https://soltoshidice.wtf/verification/verify-fairness.mjs
No access to the private application repository is needed.
Offchain Workers coordinate sessions/views and submit transactions; they are not Solana programs. Inspect games/index.json execution roles and native program receipts separately.
Bank upgrade authority was permanently revoked at finalized slot 459508498. Poker, Fights, RPS and Perps remain upgradeable; existing bank administrative controls remain. Current receipt: https://soltoshidice.wtf/verification/bank-upgrade-authority-revocation.json
Archived result files predate that revocation and retain their historical authority wording. Use current ProgramData evidence for authority status, separately from game replay.
See games/index.json and each result for exact limitations.
