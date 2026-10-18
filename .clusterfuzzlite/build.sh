#!/bin/bash -eu
# Builds the cargo-fuzz targets in fuzz/ and copies them where ClusterFuzzLite expects them.

cd "$SRC/moneylover-mcp"
cargo fuzz build -O --fuzz-dir fuzz

for target in fuzz/fuzz_targets/*.rs; do
  name="$(basename "$target" .rs)"
  cp "fuzz/target/x86_64-unknown-linux-gnu/release/$name" "$OUT/"
done
