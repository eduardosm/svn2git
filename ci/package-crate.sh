#!/usr/bin/env bash
set -euo pipefail

. ci/utils.sh

mkdir checkout
find . -mindepth 1 -maxdepth 1 -not -name checkout -print0 | xargs -0 mv -t checkout
cd checkout

pkgs_dir="$(pwd)/../packages"
out_dir="../output"

begin_group "Fetch dependencies"
cargo fetch --locked
end_group

begin_group "Vendor dependencies"
mkdir ../.cargo
cargo vendor --frozen "$pkgs_dir" > ../.cargo/config.toml
end_group

mkdir "$out_dir"

crate=svn2git
version="$(crate_version "$crate")"

begin_group "Package $crate"
cargo package -p "$crate" --frozen
tar -xf "target/package/$crate-$version.crate" -C "$pkgs_dir"
pkg_checksum="$(sha256sum "target/package/$crate-$version.crate" | awk '{print $1}')"
echo "{\"files\":{},\"package\":\"$pkg_checksum\"}" > "$pkgs_dir/$crate-$version/.cargo-checksum.json"
cp -t "$out_dir" "target/package/$crate-$version.crate"
end_group
