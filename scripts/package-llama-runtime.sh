#!/usr/bin/env bash
set -euo pipefail

# Builds a reproducible, local-only llama.cpp runtime. Pass a reviewed immutable commit SHA;
# never build releases from a floating branch. The output checksum is signed by our release key.
: "${LLAMA_CPP_REF:?Set LLAMA_CPP_REF to a reviewed llama.cpp commit SHA}"
case "$LLAMA_CPP_REF" in
  [0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f]) ;;
  *) echo "LLAMA_CPP_REF must be a 40-character lowercase commit SHA" >&2; exit 2 ;;
esac

root="$(cd "$(dirname "$0")/.." && pwd)"
work="${TMPDIR:-/tmp}/enclave-llama-cpp"
out="$root/release/runtime/$(uname -s | tr '[:upper:]' '[:lower:]')-$(uname -m)"
rm -rf "$work" "$out"
git clone --filter=blob:none https://github.com/ggml-org/llama.cpp.git "$work"
git -C "$work" checkout --detach "$LLAMA_CPP_REF"
test "$(git -C "$work" rev-parse HEAD)" = "$LLAMA_CPP_REF"
cmake -S "$work" -B "$work/build" -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=OFF
cmake --build "$work/build" --config Release --target llama-cli -j 4
mkdir -p "$out"
cp "$work/build/bin/llama-cli" "$out/llama-cli"
cp "$work/LICENSE" "$out/LICENSE-llama.cpp"
shasum -a 256 "$out/llama-cli" > "$out/SHA256SUMS"
printf '%s\n' "$LLAMA_CPP_REF" > "$out/LLAMA_CPP_COMMIT"
echo "Packaged $out. Sign SHA256SUMS in the release pipeline before distribution."
