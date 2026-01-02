#!/usr/bin/env bash
set -euo pipefail

CARGO_TOML="Cargo.toml"

# Get current version
current=$(grep -m1 '^version = ' "$CARGO_TOML" | sed 's/version = "\(.*\)"/\1/')
IFS='.' read -r major minor patch <<< "$current"

case "${1:-}" in
  patch) new="$major.$minor.$((patch + 1))" ;;
  minor) new="$major.$((minor + 1)).0" ;;
  major) new="$((major + 1)).0.0" ;;
  [0-9]*) new="$1" ;;
  *)
    echo "Usage: $0 <patch|minor|major|X.Y.Z>"
    echo "Current version: $current"
    exit 1
    ;;
esac

echo "$current -> $new"

# Update Cargo.toml
sed -i "s/^version = \"$current\"/version = \"$new\"/" "$CARGO_TOML"

# Update Cargo.lock
cargo generate-lockfile --quiet

# Commit and tag
git add "$CARGO_TOML" Cargo.lock
git commit -m "Release $new"
git tag "v$new"

echo "Released $new (run 'git push && git push --tags' to publish)"
