# RPS and fighting bracket tournaments

Separate mainnet program: `EmAQwVNSBkKUDPziofdhSNdD4HYacQSYyfuDn7c4QKe2`.
Deployed at finalized slot 459778651. See [receipts and exact hash](mainnet.json).
The existing poker, cash RPS/fights and bank programs were not upgraded.
Poker retains four tables / 40 entrants. This program has its own upgrade authority.

These are the exact normalized source files used by the tested SBF build;
[source.json](source.json) lists every SHA-256. Build from chain/bracket-program
with Solana cargo build-sbf and the committed Cargo.lock. Relative sibling
modules are included. Deployed binary SHA-256: `c5749486b42a375f3fb985251cad65a172ce7262e3bd02ac443301bcda6fc9d4`.
This is source/deployment evidence, not a claim of an independent explorer
reproducible-build verification or external security audit for this new program.

Two independent single-elimination competitions, third-place playoff,
50/30/20 prizes. Creators select 2–100 players, best of 1/3/5/7/9, minimum
100,000 SDICE sponsored prize, and entry from zero to the prize amount.
Entries join the prize pool. Registration starts immediately and closes
15 minutes after the scheduled start; starts are at most 24 hours ahead.
Play lasts up to the selected 1–24 hours after registration closes.

See [devnet receipts](devnet-tests.json): two four-player best-of-three events,
all matches completed, both playoffs and six exact payouts. Compiled SBF
tests also exercise full 100-player brackets. These are functional tests,
not a 100-person live load test. RPS uses player commit/reveal; bracket seeding
and fighting use ORAO VRF. A separate fee-only keeper advances deadlines and
settlements; it does not possess program upgrade authority.
