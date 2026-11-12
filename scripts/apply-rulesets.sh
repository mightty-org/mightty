#!/bin/sh
# Creates or updates the GitHub rulesets in .github/rulesets/*.json, matched
# by name. Needs `gh` signed in as an admin of the repository.
# usage: scripts/apply-rulesets.sh [OWNER/REPO, default mightty-org/mightty]
set -eu
cd "$(dirname "$0")/.."
repo=${1:-mightty-org/mightty}
for file in .github/rulesets/*.json; do
  name=$(python3 -I -c 'import json, sys; print(json.load(open(sys.argv[1]))["name"])' "$file")
  id=$(gh api "repos/$repo/rulesets" --jq ".[] | select(.name == \"$name\") | .id")
  if [ -n "$id" ]; then
    gh api --method PUT "repos/$repo/rulesets/$id" --input "$file" >/dev/null
    echo "updated: $name"
  else
    gh api --method POST "repos/$repo/rulesets" --input "$file" >/dev/null
    echo "created: $name"
  fi
done
