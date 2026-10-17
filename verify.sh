#!/usr/bin/env bash
# Read-only verification: no keypair, transaction submission or deployment.
set -euo pipefail
cd "$(dirname "$0")"
image=solanafoundation/solana-verifiable-build@sha256:f71be5ca7620b7e40933b7f1294fa44e01d08c1fc5ba1f375a2478f5a01580d3
extra=()
case "${1:?Choose bank, poker, fights, rps or perps}" in
  bank) id=CuTLp7pDmNGkFgi4aoh8Ef1YSjc2BzECQRLzYqaoVWBR; mount=games/chain; workspace=games/chain/program; lib=hooddice_bank;;
  poker) id=3EB6JJ2k1yPdg9qokViBdSWq4jaEVG4wiKbS8Pw3FmWQ; mount=games/poker; workspace=$mount; lib=riverproof;;
  fights) id=5PrHPCCbKpW4SvBZfXmt23RKMaShSM4XGTJdBh8qpw3q; mount=games/fights; workspace=games/fights/fight-program; lib=hooddice_fights;;
  rps) id=96hGHn4JD3AyzXvNPdx4RyNtS8y7sQ5NizagMsdMAXhG; mount=games/chain/rps-program; workspace=$mount; lib=hooddice_rps;;
  perps) id=7YWUnPaP5tQjJTC5kZu235P5Qix2VpoJk5rTMgkXeCw7; mount=perps; workspace=$mount; lib=acidpump_er; extra=(-- --features mainnet);;
  *) echo 'Unknown program' >&2; exit 2;;
esac
solana-verify build "$PWD/$mount" --workspace-path "$PWD/$workspace" --library-name "$lib" --base-image "$image" "${extra[@]}"
local_hash=$(solana-verify get-executable-hash "$workspace/target/deploy/$lib.so")
chain_hash=$(solana-verify get-program-hash -u "${VERIFICATION_RPC_URL:-https://api.mainnet-beta.solana.com}" "$id")
printf 'Local: %s\nChain: %s\n' "$local_hash" "$chain_hash"
test "$local_hash" = "$chain_hash" || { echo 'HASH MISMATCH: not verified.' >&2; exit 1; }
printf '%s matches the current mainnet executable.\n' "$id"
