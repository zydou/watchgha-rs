#!/usr/bin/env bash
# Create the repository rulesets that protect `main` and the release tags.
#
# Usage: bash scripts/init-rulesets.sh
#
# Run after the initial push to the repository (see CLAUDE.local.md §10.4):
# the `pull_request` rule would otherwise block that first push. Safe to
# re-run: a ruleset with the same name is updated in place.
#
# The required-check contexts are the job names in .github/workflows/release.yml;
# keep them in sync when the target matrix changes.
set -euo pipefail

repo="$(gh repo view --json nameWithOwner --jq .nameWithOwner)"

apply_ruleset() {
  local name="$1"
  local id
  id="$(gh api "/repos/$repo/rulesets" --jq "[.[] | select(.name == \"$name\") | .id][0] // empty")"
  if [[ -n $id ]]; then
    gh api --method PUT "/repos/$repo/rulesets/$id" --input - > /dev/null
    echo "ruleset '$name' updated (id $id)"
  else
    gh api --method POST "/repos/$repo/rulesets" --input - > /dev/null
    echo "ruleset '$name' created"
  fi
}

apply_ruleset main << 'EOF'
{
  "name": "main",
  "target": "branch",
  "enforcement": "active",
  "conditions": {
    "ref_name": { "include": ["refs/heads/main"], "exclude": [] }
  },
  "rules": [
    {
      "type": "pull_request",
      "parameters": {
        "required_approving_review_count": 0,
        "dismiss_stale_reviews_on_push": false,
        "require_code_owner_review": false,
        "require_last_push_approval": false,
        "required_review_thread_resolution": true,
        "allowed_merge_methods": ["rebase", "merge"]
      }
    },
    {
      "type": "required_status_checks",
      "parameters": {
        "strict_required_status_checks_policy": true,
        "required_status_checks": [
          { "context": "fmt & clippy" },
          { "context": "x86_64-unknown-linux-gnu" },
          { "context": "x86_64-unknown-linux-musl" },
          { "context": "aarch64-unknown-linux-gnu" },
          { "context": "aarch64-unknown-linux-musl" },
          { "context": "x86_64-apple-darwin" },
          { "context": "aarch64-apple-darwin" }
        ]
      }
    },
    { "type": "required_signatures" },
    { "type": "non_fast_forward" },
    { "type": "deletion" }
  ]
}
EOF

apply_ruleset tags << 'EOF'
{
  "name": "tags",
  "target": "tag",
  "enforcement": "active",
  "conditions": {
    "ref_name": { "include": ["refs/tags/*"], "exclude": [] }
  },
  "rules": [
    { "type": "non_fast_forward" },
    { "type": "deletion" }
  ]
}
EOF

echo "rulesets on $repo:"
gh api "/repos/$repo/rulesets" --jq '.[] | [.name, .enforcement]'
