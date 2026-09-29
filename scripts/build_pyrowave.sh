#!/usr/bin/env bash
# Build and install the PyroWave C API (libpyrowave-shared-0.dll) with MSYS2 UCRT64 MinGW-w64.
#
# Usage (MSYS2 UCRT64 shell):
#   scripts/build_pyrowave.sh [commit] [prefix]
#     commit  full SHA of https://github.com/Themaister/pyrowave
#             (default: the commit pinned below; its bitstream matches the PyroWave
#             Moonlight clients)
#     prefix  install prefix (default: $PWD/pyrowave-install)
#
# Environment:
#   PYROWAVE_WORKDIR  sources + build tree (default: $PWD/pyrowave-work)
#   PYROWAVE_REPO     git URL (default: https://github.com/Themaister/pyrowave.git)
#   PYROWAVE_STRIP=0  keep debug symbols in the installed DLL (default: strip)
#   PYROWAVE_JOBS     parallel jobs (default: ninja's default)
#
# Needs git, gcc, cmake (>= 3.27) and ninja from UCRT64. Consumers of pyrowave.h
# also need Vulkan headers >= 1.4 (mingw-w64-ucrt-x86_64-vulkan-headers). The DLL
# loads vulkan-1.dll at runtime through volk, so no Vulkan loader is linked.
#
# The prefix gets bin/libpyrowave-shared-0.dll, lib/libpyrowave-shared.dll.a,
# include/pyrowave/pyrowave.h, share/pyrowave-shared/cmake (imported target
# pyrowave-shared) and share/licenses/pyrowave (MIT notices of PyroWave, Granite
# and volk). Re-runnable: the checkout is moved to the requested commit and the
# build tree is reused.

set -euo pipefail

PINNED_COMMIT=89f7e47d4abbf650c91fae766728af866c5e32a0
COMMIT=${1:-$PINNED_COMMIT}
PREFIX=${2:-$PWD/pyrowave-install}
WORKDIR=${PYROWAVE_WORKDIR:-$PWD/pyrowave-work}
REPO=${PYROWAVE_REPO:-https://github.com/Themaister/pyrowave.git}

if [[ "${MSYSTEM:-}" != "UCRT64" ]]; then
	echo "error: run this from an MSYS2 UCRT64 shell (MSYSTEM=UCRT64), got '${MSYSTEM:-unset}'" >&2
	exit 1
fi
if [[ ! "$COMMIT" =~ ^[0-9a-f]{40}$ ]]; then
	echo "error: commit must be a full 40-character SHA, got '$COMMIT'" >&2
	exit 1
fi

mkdir -p "$WORKDIR" "$PREFIX"
WORKDIR=$(cd "$WORKDIR" && pwd)
PREFIX=$(cd "$PREFIX" && pwd)
SRC=$WORKDIR/pyrowave
BUILD=$WORKDIR/build

# 1. PyroWave at the exact commit (shallow; GitHub serves reachable SHAs directly).
if [[ ! -d "$SRC/.git" ]]; then
	git init -q "$SRC"
	git -C "$SRC" remote add origin "$REPO"
fi
git -C "$SRC" remote set-url origin "$REPO"
git -C "$SRC" fetch -q --depth 1 origin "$COMMIT"
git -C "$SRC" -c advice.detachedHead=false checkout -q --force FETCH_HEAD
PYROWAVE_SHA=$(git -C "$SRC" rev-parse HEAD)
if [[ "$PYROWAVE_SHA" != "$COMMIT" ]]; then
	echo "error: checked out $PYROWAVE_SHA, expected $COMMIT" >&2
	exit 1
fi
echo "pyrowave: $PYROWAVE_SHA"

# 2. The Granite subset exactly as upstream pins it: GRANITE_COMMIT from
#    checkout_granite.sh plus only its volk and Vulkan-Headers submodules.
GRANITE_COMMIT=$(sed -n 's/^GRANITE_COMMIT=//p' "$SRC/checkout_granite.sh")
if [[ ! "$GRANITE_COMMIT" =~ ^[0-9a-f]{40}$ ]]; then
	echo "error: could not read GRANITE_COMMIT from checkout_granite.sh" >&2
	exit 1
fi
(cd "$SRC" && bash ./checkout_granite.sh)
GRANITE_SHA=$(git -C "$SRC/Granite" rev-parse HEAD)
if [[ "$GRANITE_SHA" != "$GRANITE_COMMIT" ]]; then
	echo "error: Granite is at $GRANITE_SHA, expected $GRANITE_COMMIT" >&2
	exit 1
fi
echo "Granite: $GRANITE_SHA"

# 3. Configure. The defaults build the shared C API; PYROWAVE_DEVEL/UTILS would
#    pull in the full Granite (SDL, glslang, ...).
cmake -S "$SRC" -B "$BUILD" -G Ninja \
	-DCMAKE_BUILD_TYPE=Release \
	-DCMAKE_INSTALL_PREFIX="$PREFIX" \
	-DPYROWAVE_DEVEL=OFF \
	-DPYROWAVE_UTILS=OFF

# 4. Build and install.
cmake --build "$BUILD" ${PYROWAVE_JOBS:+--parallel "$PYROWAVE_JOBS"}
if [[ "${PYROWAVE_STRIP:-1}" == "1" ]]; then
	cmake --install "$BUILD" --strip
else
	cmake --install "$BUILD"
fi

# 5. MIT notices for everything linked into the DLL.
LICENSE_DIR=$PREFIX/share/licenses/pyrowave
mkdir -p "$LICENSE_DIR"
cp "$SRC/LICENSE" "$LICENSE_DIR/LICENSE"
cp "$SRC/Granite/LICENSE" "$LICENSE_DIR/LICENSE.Granite"
cp "$SRC/Granite/third_party/volk/LICENSE.md" "$LICENSE_DIR/LICENSE.volk.md"

API_VERSION=$(sed -n 's/^#define PYROWAVE_API_VERSION_\(MAJOR\|MINOR\|PATCH\) \([0-9]*\)$/\2/p' "$SRC/pyrowave.h" | paste -sd.)
mkdir -p "$PREFIX/share/pyrowave-shared"
cat > "$PREFIX/share/pyrowave-shared/build-info.txt" <<EOF
pyrowave_commit=$PYROWAVE_SHA
granite_commit=$GRANITE_SHA
api_version=$API_VERSION
compiler=$(gcc --version | head -n1)
cmake=$(cmake --version | head -n1)
EOF

# 6. The DLL may only depend on Windows system DLLs and the UCRT: no Vulkan
#    loader link (volk loads it) and no MinGW runtime DLLs (upstream links
#    libstdc++/libgcc/winpthread statically).
shopt -s nullglob
dlls=("$PREFIX"/bin/libpyrowave-shared-*.dll)
if (( ${#dlls[@]} != 1 )); then
	echo "error: expected exactly one libpyrowave-shared-*.dll in $PREFIX/bin" >&2
	exit 1
fi
DLL=${dlls[0]}
imports=$(objdump -p "$DLL" | sed -n 's/^[[:space:]]*DLL Name: //p')
echo "Imports of $(basename "$DLL"):"
sed 's/^/  /' <<<"$imports"
if grep -Eiq '^(vulkan-1|libstdc\+\+-6|libgcc_s_.*|libwinpthread-1)\.dll$' <<<"$imports"; then
	echo "error: $(basename "$DLL") links a DLL that must not be a hard dependency" >&2
	exit 1
fi
if objdump -h "$DLL" | grep -Eq ' \.(debug|zdebug)' && [[ "${PYROWAVE_STRIP:-1}" == "1" ]]; then
	echo "error: $(basename "$DLL") still carries debug sections" >&2
	exit 1
fi

echo
echo "Installed PyroWave C API $API_VERSION ($PYROWAVE_SHA) to $PREFIX:"
(cd "$PREFIX" && find . -type f | sort | sed 's|^\./|  |')
