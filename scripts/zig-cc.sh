#!/bin/sh
# zig C 编译器包装：固定目标三元组（glibc 2.28，UOS 20 兼容），
# 并把 libyuv 的 CMake 探测出来的 -march=armv8.x/armv9.x 换成 zig 0.13 能认的 generic。
# zig 0.13 的 CPU 名表里没有 armv8.2，直接透传会报 "unknown CPU: 'armv8.2'"。
TARGET="${ZIG_TARGET:-aarch64-linux-gnu.2.28}"
out=""
for a in "$@"; do
  case "$a" in
    -march=armv8*|-mcpu=armv8*|-march=armv9*|-mcpu=armv9*) a="-mcpu=generic" ;;
  esac
  out="$out \"$a\""
done
# shellcheck disable=SC2086
eval exec /opt/zig/zig cc --target="$TARGET" $out
