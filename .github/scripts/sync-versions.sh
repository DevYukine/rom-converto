#!/usr/bin/env bash
# Syncs the version from rom-converto-cli/Cargo.toml to all other crates and tauri.conf.json.
# Called by conventional-changelog-action as a pre-commit hook.

set -euo pipefail

CLI_TOML="crates/rom-converto-cli/Cargo.toml"
VERSION=$(grep '^version = ' "$CLI_TOML" | head -1 | sed 's/version = "\(.*\)"/\1/')

echo "Syncing version $VERSION to all crates..."

# Update lib Cargo.toml
sed -i "s/^version = \".*\"/version = \"$VERSION\"/" crates/rom-converto-lib/Cargo.toml

# Update GUI src-tauri Cargo.toml
sed -i "s/^version = \".*\"/version = \"$VERSION\"/" crates/rom-converto-gui/src-tauri/Cargo.toml

# Update tauri.conf.json
sed -i "s/\"version\": \".*\"/\"version\": \"$VERSION\"/" crates/rom-converto-gui/src-tauri/tauri.conf.json

# Stage the changed files so they get included in the changelog commit
git add crates/rom-converto-lib/Cargo.toml
git add crates/rom-converto-gui/src-tauri/Cargo.toml
git add crates/rom-converto-gui/src-tauri/tauri.conf.json

echo "All versions synced to $VERSION"
