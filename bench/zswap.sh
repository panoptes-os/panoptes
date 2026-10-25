#!/usr/bin/env bash
# Set the zswap pool allocator and compressor for new entries.
# The host default before this run was zbud/lz4; restore it with
# zswap.sh zbud lz4. usage: zswap.sh ZPOOL COMPRESSOR
set -eu
echo "$1" | sudo tee /sys/module/zswap/parameters/zpool > /dev/null
echo "$2" | sudo tee /sys/module/zswap/parameters/compressor > /dev/null
echo "zswap: $(cat /sys/module/zswap/parameters/zpool)/$(cat /sys/module/zswap/parameters/compressor)"
