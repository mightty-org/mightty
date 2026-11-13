#!/bin/sh
# Makes a release in two steps, with a pull request between them.
#
#   scripts/release.sh 0.2.0    sets the version in Cargo.toml and Cargo.lock
#                               on a branch and opens a pull request
#   scripts/release.sh --tag    once that's merged: tags main with its
#                               version and pushes the tag, which starts the
#                               release workflow (see RELEASING.md)
#
# Needs `gh`, signed in to an account that may push to the repository.
set -eu
cd "$(dirname "$0")/.."

current() {
  sed -n 's/^version = "\(.*\)"/\1/p' "$1" | head -1
}

die() {
  echo "release.sh: $*" >&2
  exit 1
}

case ${1:-} in
  --tag)
    git fetch --quiet origin main --tags
    version=$(git show origin/main:Cargo.toml | current /dev/stdin)
    [ -n "$version" ] || die "no version in Cargo.toml on main"
    if git rev-parse --quiet --verify "refs/tags/v$version" >/dev/null; then
      die "v$version is tagged already: is the version's pull request merged?"
    fi
    git tag "v$version" origin/main
    git push origin "v$version"
    echo "pushed v$version; approve and follow the run:"
    echo "  https://github.com/mightty-org/mightty/actions/workflows/release.yml"
    ;;
  [0-9]*)
    version=$1
    # Three numbers, as the update check reads versions; a pre-release
    # (0.2.0-beta.1) is tagged by hand.
    echo "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' || die "$version isn't a version like 1.2.3"
    [ -z "$(git status --porcelain)" ] || die "commit or stash your changes first"
    git fetch --quiet origin main --tags
    old=$(git show origin/main:Cargo.toml | current /dev/stdin)
    [ "$version" != "$old" ] || die "main is $old already"
    if git rev-parse --quiet --verify "refs/tags/v$version" >/dev/null; then
      die "v$version is tagged already"
    fi
    branch=release-$version
    git checkout --quiet -b "$branch" origin/main
    sed -i '' "s/^version = \"$old\"/version = \"$version\"/" Cargo.toml
    [ "$(current Cargo.toml)" = "$version" ] || die "couldn't set the version in Cargo.toml"
    # The lock file names each crate's version too.
    cargo update --quiet --workspace
    git commit --quiet -am "Version $version"
    git push --quiet -u origin "$branch"
    gh pr create --base main --head "$branch" --title "Version $version" \
      --body "Sets the version to $version, for the release. Once this is merged, \`scripts/release.sh --tag\` tags it and starts the release workflow."
    echo "when it's merged: scripts/release.sh --tag"
    ;;
  *)
    sed -n '2,10p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
    ;;
esac
