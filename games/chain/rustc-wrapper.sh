#!/usr/bin/env bash
# Reproduce the original release's Cargo crate identifiers across build paths.
# Only compiler metadata is pinned; compiler inputs/outputs are never patched.
set -euo pipefail
compiler="$1"
shift
args=("$@")
metadata=""
for ((i=0;i<${#args[@]}-1;i++)); do
  if [[ "${args[$i]}" = --crate-name ]]; then
    case "${args[$((i+1))]}" in
      sdice_slots_rules) metadata=6d87b0c90b6bb6d4;;
      hooddice_bank) metadata=b8ceeb93c4edb71b;;
    esac
  fi
done
if [[ -n "$metadata" ]]; then
  for ((i=0;i<${#args[@]};i++)); do
    if [[ "${args[$i]}" = metadata=* ]]; then args[$i]="metadata=$metadata"; fi
  done
fi
exec "$compiler" "${args[@]}"
