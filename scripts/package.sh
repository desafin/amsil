#!/bin/sh
# 릴리스 묶음을 만든다: 실행 파일, 그 옆에 두는 라이브러리, 모델 받기 스크립트, 안내 문서.
# 쓰는 법: scripts/package.sh [빌드 폴더 (기본 target/release)] [결과를 둘 폴더 (기본 dist)]
# 필요한 도구: ldd, tar, sha256sum
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

# 실행 파일 옆에 두는 라이브러리. x265는 배포판마다 버전 이름이 달라서 함께 넣지 않으면
# 다른 배포판에서 실행되지 않는다. 나머지는 깔려 있지 않은 컴퓨터를 위해 넣는다.
: > "$stage/BUNDLED-LIBS.txt"
for lib in libwebgpu_dawn libx265 libde265 libnuma; do
    line=$(ldd "$build/amsil" | awk -v lib="$lib" 'index($1, lib ".so") == 1 { print $1, $3; exit }')
    soname=${line%% *}
    path=${line#* }
    if [ -z "$line" ] || [ ! -e "$path" ]; then
        echo "$lib: 실행 파일이 쓰는 라이브러리를 찾지 못했습니다" >&2
        exit 1
    fi
    # 심볼릭 링크가 아니라 실제 파일을, 실행 파일이 찾는 이름으로 넣는다.
    cp -L "$path" "$stage/$soname"
    # 어느 패키지의 어느 버전인지 적어 둔다 (dpkg가 있는 배포판에서만).
    package=$(dpkg -S "$(readlink -f "$path")" 2>/dev/null | cut -d: -f1 || true)
    if [ -n "$package" ]; then
        echo "$soname: $package $(dpkg-query -W -f '${Version}' "$package")" >> "$stage/BUNDLED-LIBS.txt"
    else
        echo "$soname: (배포판 패키지가 아님)" >> "$stage/BUNDLED-LIBS.txt"
    fi
done

cp "$root/scripts/fetch-models.sh" "$stage/fetch-models.sh"
cp "$root/README.md" "$root/LICENSE" "$root/THIRD-PARTY.md" "$stage/"
chmod +x "$stage/amsil" "$stage/fetch-models.sh"

tar -C "$out" -czf "$out/$name.tar.gz" "$name"
(cd "$out" && sha256sum "$name.tar.gz" > "$name.tar.gz.sha256")
echo "만들었습니다: $out/$name.tar.gz"
cat "$stage/BUNDLED-LIBS.txt"
