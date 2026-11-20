# RCD 정답지 생성기

`src/rcd.rs`(RCD 디모자익을 Rust로 옮긴 것)가 원본과 같은 결과를 내는지 확인하는 데 쓰는 정답지(`tests/data/rcd/*.bin`)를 만든다. 정답지는 저장소에 들어 있으므로, 테스트를 돌리는 데는 이 폴더가 필요 없다.

## 들어 있는 것

| 파일 | 내용 |
|---|---|
| `rcd_demosaicing.c` | 원본 RCD, 고치지 않은 사본. Luis Sanz Rodríguez, Release 2.3 @ 171125. <https://github.com/LuisSR/RCD-Demosaicing> 커밋 `0aa9d6e234f5`. 라이선스 GPL-3.0 |
| `harness.c` | 원본을 dcraw 밖에서 돌리는 틀. dcraw의 전역 변수와 도우미를 흉내 내고, `border_interpolate()`는 dcraw.c(Dave Coffin)에서 그대로 옮겼다. 시험 그림(무작위 값, 경계·가는 선·포화 조각)도 여기서 만든다 |
| `make-fixtures.sh` | 임시 폴더에 복사해 gcc로 빌드하고, Bayer 배치 4가지 × 그림 2가지를 47×41로 만들어 `tests/data/rcd/`에 쓴다 |

## 다시 만들기

```
scripts/rcd-reference/make-fixtures.sh
```

gcc가 필요하다. 부동소수점 계산을 정해진 대로 하게 `-ffp-contract=off -fno-fast-math`로 빌드한다. 같은 머신에서 몇 번을 만들어도 같은 파일이 나온다.

## 정답지 형식

리틀 엔디언. `"RCD1"`, `u32` 가로, `u32` 세로, `u32` filters(dcraw 형식), `u16` × (가로·세로) 입력 CFA, `u16` × (가로·세로·3) 출력 RGB(화소마다 R, G, B).

원본은 시작 열을 정하는 식의 연산자 우선순위 때문에 0~3열도 처리하며 앞 줄의 끝을 읽고, 4행에서 버퍼 앞을 한 번 읽는다. 틀은 버퍼 앞에 0을 채운 여유를 두어 결과를 결정적으로 만든다. `src/rcd.rs`는 이 두 가지를 고쳐서 옮겼고, 그 영향은 8열까지다. 그래서 테스트는 가장자리 10화소를 뺀 안쪽만 비교한다.
