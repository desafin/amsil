#!/usr/bin/env python3
"""카메라 JPEG가 굵기(공간 주파수)마다 무엇을 하는지: 세부가 있는 곳과 평평한 곳을 나눠서.

샤픈 없이 현상한 그림(`*-off-pre.raw`)에 대한 카메라 JPEG의 진폭 비율이다. 1보다 크면 카메라가 그 굵기를
키운 것(샤픈), 작으면 깎은 것(노이즈 감소)이다. 결과는 `camera_curve.json`에 써서 fit_filter.py가 쓴다.

    cargo test --release -- --ignored render_for_sharpness_measurement
    python3 scripts/sharpness/camera_curve.py [폴더]
"""
import json
import sys

sys.path.insert(0, str(__import__("pathlib").Path(__file__).parent))
from common import CENTRES, PHOTOS, at, camera_jpeg, curve, data_dir, load_raw, luma, split  # noqa: E402

SHOW = [0.05, 0.1, 0.15, 0.2, 0.25, 0.3, 0.35, 0.4, 0.45]


def main():
    folder = data_dir()
    result = {"frequencies": CENTRES.tolist(), "photos": {}}
    print("카메라 JPEG / 샤픈 없는 현상, 진폭 비율 (cycles/pixel)")
    print(f"{'사진':10s} {'구역':6s}" + "".join(f"{f:>7.2f}" for f in SHOW))
    for name in PHOTOS:
        camera = luma(camera_jpeg(name))
        plain = luma(load_raw(folder / f"{name}-off-pre.raw"))
        detail, flat = split(plain)
        curves = {"detail": curve(camera, plain, detail), "flat": curve(camera, plain, flat)}
        for kind, g in curves.items():
            print(f"{name:10s} {kind:6s}" + "".join(f"{at(g, f):7.2f}" for f in SHOW))
        result["photos"][name] = {kind: g.tolist() for kind, g in curves.items()}
    (folder / "camera_curve.json").write_text(json.dumps(result))
    print(f"\n{folder / 'camera_curve.json'}")


if __name__ == "__main__":
    main()
