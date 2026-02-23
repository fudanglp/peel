version := `grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)".*/\1/'`

# Show current version
current:
    @echo "v{{version}}"

# Bump version, regenerate CI, commit, tag, and push
release new_version:
    @echo "Releasing v{{new_version}} (current: v{{version}})"
    sed -i 's/^version = ".*"/version = "{{new_version}}"/' Cargo.toml
    cargo check
    cargo dist generate
    git add Cargo.toml Cargo.lock .github/
    git commit -m "Release v{{new_version}}"
    git tag "v{{new_version}}"
    git push && git push --tags
    @echo "Done — release workflow will build artifacts."
