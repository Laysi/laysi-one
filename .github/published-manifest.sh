#!/bin/sh
# Copies the release manifest already published for a tag into a file.
#
#   sh .github/published-manifest.sh v0.1.13 published.txt
#
# Leaves the file absent when the tag has no release or the release has no
# manifest. Any other answer from GitHub -- an API, network or permission
# error -- fails the script: release.yml asks this to decide whether a
# version was already published, and an error read as "not published" would
# let a second build go out under that version.

set -eu

tag=${1:?usage: published-manifest.sh <tag> <out>}
out=${2:?usage: published-manifest.sh <tag> <out>}
repo=${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is not set}

rm -f "$out"

# Every release rather than a lookup by tag: a missing release and a failed
# request would otherwise both be a non-zero exit, and only the second may
# pass for "none".
tags=$(gh api --paginate "repos/$repo/releases" --jq '.[].tag_name')
printf '%s\n' "$tags" | grep -qxF -- "$tag" || exit 0

assets=$(gh release view "$tag" --repo "$repo" --json assets --jq '.assets[].name')
printf '%s\n' "$assets" | grep -qxF release-manifest.txt || exit 0

dir=$(mktemp -d)
trap 'rm -rf "$dir"' EXIT
gh release download "$tag" --repo "$repo" --pattern release-manifest.txt --dir "$dir"
mv "$dir/release-manifest.txt" "$out"
