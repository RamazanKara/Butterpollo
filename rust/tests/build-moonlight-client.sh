#!/bin/sh
# Independent Linux receiver. Requires CMake, C/C++ compilers, OpenSSL, FFmpeg
# (avcodec/avutil) and Opus development packages. No display server is needed.
set -eu
if [ "$#" -ne 2 ]; then
    echo "usage: $0 ARTIFACT_DIRECTORY MOONLIGHT_SOURCE_DIRECTORY" >&2
    exit 2
fi
mkdir -p "$1"
artifact=$(cd "$1" && pwd)
source=$(cd "$2" && pwd)
scripts=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
cmake -S "$source" -B "$artifact/moonlight-client-build" \
    -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=OFF
cmake --build "$artifact/moonlight-client-build" --parallel 2
cc -O2 "$scripts/moonlight_client.c" -I"$source/src" \
    "$artifact/moonlight-client-build/libmoonlight-common-c.a" \
    "$artifact/moonlight-client-build/enet/libenet.a" \
    $(pkg-config --cflags --libs libavcodec libavutil opus openssl) \
    -lpthread -lm -o "$artifact/moonlight-client"
echo "Independent client: $artifact/moonlight-client"
