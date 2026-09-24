#!/usr/bin/env bash
set -euo pipefail

# Build the project.
build() {
  echo "building"
  cargo build --release
  echo "done"
}

deploy() {
  build
  scp target/release/app host:/srv
  ssh host restart
}

build
