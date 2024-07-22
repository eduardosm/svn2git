#!/usr/bin/env bash
set -euo pipefail

. ci/utils.sh

begin_group "Fetch dependencies"
cargo fetch --locked
end_group

export CARGO_REGISTRY_TOKEN="$CRATES_IO_TOKEN"

crate="svn2git"

begin_group "Publish $crate"
cargo publish -p "$crate" --no-verify --locked
end_group
