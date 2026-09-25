#!/usr/bin/env bash
# Render every .mmd next to this script to .svg. Needs mmdc (brew install mermaid-cli).
set -o errexit -o nounset -o pipefail
cd "$(dirname "$0")"
for f in *.mmd; do
  mmdc -q -i "$f" -o "${f%.mmd}.svg" -c mermaid.json -b white
  echo "${f%.mmd}.svg"
done
