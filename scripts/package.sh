#!/bin/sh
# Builds the release bundle: the executable, the libraries shipped next to it, the model download script, and docs.
# Usage: scripts/package.sh [build dir (default target/release)] [output dir (default dist)]
# Requires: ldd, tar, sha256sum
set -eu

build="${1:-target/release}"
out="${2:-dist}"
root=$(cd "$(dirname "$0")/.." && pwd)
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -n 1)
name="amsil-$version-linux-x86_64"
stage="$out/$name"

rm -rf "$stage"
mkdir -p "$stage"
cp "$build/amsil" "$stage/amsil"

# Libraries shipped next to the executable. The x265 soname differs between distributions, so without
# bundling it the binary won't run on other distros. The rest are for systems that don't have them installed.
: > "$stage/BUNDLED-LIBS.txt"
for lib in libwebgpu_dawn libx265 libde265 libnuma; do
    line=$(ldd "$build/amsil" | awk -v lib="$lib" 'index($1, lib ".so") == 1 { print $1, $3; exit }')
    soname=${line%% *}
    path=${line#* }
    if [ -z "$line" ] || [ ! -e "$path" ]; then
        echo "$lib: library used by the executable not found" >&2
        exit 1
    fi
    # Copy the real file, not the symlink, under the name the executable looks for.
    cp -L "$path" "$stage/$soname"
    # Record the source package and version (only on distros with dpkg).
    package=$(dpkg -S "$(readlink -f "$path")" 2>/dev/null | cut -d: -f1 || true)
    if [ -n "$package" ]; then
        echo "$soname: $package $(dpkg-query -W -f '${Version}' "$package")" >> "$stage/BUNDLED-LIBS.txt"
    else
        echo "$soname: (not from a distro package)" >> "$stage/BUNDLED-LIBS.txt"
    fi
done

cp "$root/scripts/fetch-models.sh" "$stage/fetch-models.sh"
cp "$root/README.md" "$root/README.en.md" "$root/LICENSE" "$root/THIRD-PARTY.md" "$stage/"
chmod +x "$stage/amsil" "$stage/fetch-models.sh"

tar -C "$out" -czf "$out/$name.tar.gz" "$name"
(cd "$out" && sha256sum "$name.tar.gz" > "$name.tar.gz.sha256")
echo "Created: $out/$name.tar.gz"
cat "$stage/BUNDLED-LIBS.txt"
