#!/bin/sh
# Prints the tag a release's notes count from. Empty for the first one.
#
#   sh .github/previous-tag.sh v0.2.0-rc.2
#
# A final release counts from the previous final release, so its page lists
# everything since the version a customer last installed rather than what
# changed after its last release candidate. A release candidate counts from
# whatever tag came before it, final or not, so each one shows what the last
# round of checking did not see.
#
# A tag with a `-` is a release candidate: ReleaseVersion takes no build
# metadata, so a hyphen only ever opens a prerelease. versionsort.suffix puts
# v0.2.0-rc.1 before v0.2.0, where plain version order puts it after.

set -eu

tag=${1:?usage: previous-tag.sh <tag>}

case $tag in
*-*) tags=$(git -c versionsort.suffix=- tag --list 'v*' --sort=-v:refname) ;;
*) tags=$(git tag --list 'v*' --sort=-v:refname | grep -v -e - || true) ;;
esac

printf '%s\n' "$tags" | grep -A1 -xF -e "$tag" | sed -n 2p
