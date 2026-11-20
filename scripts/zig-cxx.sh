#!/bin/sh
# zig C++ 编译器包装：同 zig-cc.sh，另加 armv8.2 CPU 参数过滤。
TARGET="${ZIG_TARGET:-aarch64-linux-gnu.2.28}"
out=""
for a in "$@"; do
  case "$a" in
    -march=armv8*|-mcpu=armv8*|-march=armv9*|-mcpu=armv9*) a="-mcpu=generic" ;;
  esac
  out="$out \"$a\""
done
# shellcheck disable=SC2086
eval exec /opt/zig/zig c++ --target="$TARGET" $out
