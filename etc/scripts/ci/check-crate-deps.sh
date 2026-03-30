#!/usr/bin/env bash
set -euo pipefail

# Disallowed crate dependency rules: "source:target"
# Crates in crates/<source>/ must not depend on crates in crates/<target>/
DISALLOWED_DEPS=(
  "utilities:client"
  "utilities:builder"
  "utilities:consensus"
  "client:infra"
  "utilities:infra"
  "builder:infra"
  "consensus:infra"
  "core:execution"
)

# Allowed exceptions for legacy rules.
# These are foundational consensus protocol crates that are local path deps under crates/consensus/.
ALLOWED_DEPS=(
  "base-consensus-genesis"
  "base-consensus-registry"
  "base-consensus-engine"
)

build_allowed_filter() {
  local rule="$1"

  case "$rule" in
    core:execution)
      printf '[]'
      ;;
    *)
      local allowed_filter
      allowed_filter=$(printf '"%s",' "${ALLOWED_DEPS[@]}")
      printf '[%s]' "${allowed_filter%,}"
      ;;
  esac
}

# Fetch cargo metadata once, ensuring Cargo.lock is in sync
METADATA=$(cargo metadata --format-version 1 --no-deps --locked)

FOUND_VIOLATIONS=false

for rule in "${DISALLOWED_DEPS[@]}"; do
  SOURCE="${rule%%:*}"
  TARGET="${rule##*:}"
  ALLOWED_FILTER=$(build_allowed_filter "$rule")

  VIOLATIONS=$(echo "$METADATA" | jq -r --argjson allowed "$ALLOWED_FILTER" "
    [.packages[]
     | select(.manifest_path | contains(\"/crates/$SOURCE/\"))
     | . as \$pkg
     | .dependencies[]
     | select(.path)
     | select(.path | contains(\"/crates/$TARGET/\"))
     | select(.name as \$n | \$allowed | index(\$n) | not)
     | \"\(\$pkg.name) -> \(.name)\"
    ]
    | .[]
  ")

  if [ -n "$VIOLATIONS" ]; then
    echo "ERROR: Found $SOURCE -> $TARGET dependency violations:"
    echo "$VIOLATIONS" | while read -r violation; do
      echo "  - $violation"
    done
    echo ""
    FOUND_VIOLATIONS=true
  fi
done

if [ "$FOUND_VIOLATIONS" = true ]; then
  echo "Dependency rules are defined in etc/scripts/ci/check-crate-deps.sh"
  exit 1
fi

echo "All crate dependencies are valid"
