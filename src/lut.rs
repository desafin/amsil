//! 3D LUT (`.cube` 파일): 색을 다른 색으로 바꾸는 표. 필름 느낌 같은 "룩"을 입히는 데 쓴다.

use crate::lang::t;
use crate::{Image, Result};
use rayon::prelude::*;
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub struct Lut {
    /// 한 축의 격자 수.
    size: usize,
    /// 입력 값의 범위 (채널별 최소, 최대).
    domain: ([f32; 3], [f32; 3]),
    /// 격자점의 출력 색. 빨강이 가장 빨리 변하는 순서 (`.cube`의 순서 그대로).
    table: Vec<[f32; 3]>,
}

impl Lut {
    /// `.cube` 형식의 글을 읽는다.
    pub fn parse(text: &str) -> Result<Lut> {
        let mut size = None;
        let mut domain = ([0.0; 3], [1.0; 3]);
        let mut table = Vec::new();
        for (index, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let unreadable = || {
                t(
                    format!("{}번째 줄을 읽을 수 없습니다: {line}", index + 1),
                    format!("Line {} cannot be read: {line}", index + 1),
                )
            };
            let mut words = line.split_whitespace();
            let first = words.next().unwrap_or_default();
            let mut numbers = |count: usize| -> Result<Vec<f32>> {
                let values: Vec<f32> = words.by_ref().map_while(|word| word.parse().ok()).collect();
                if values.len() == count {
                    Ok(values)
                } else {
                    Err(unreadable().into())
                }
            };
            match first {
                "LUT_1D_SIZE" => {
                    return Err(t(
                        "1D LUT는 지원하지 않습니다. 3D LUT(.cube)만 쓸 수 있습니다",
                        "1D LUTs are not supported. Only 3D LUTs (.cube) work",
                    )
                    .into());
                }
                "LUT_3D_SIZE" => {
                    let n = numbers(1)?[0] as usize;
                    if !(2..=256).contains(&n) {
                        return Err(t(
                            format!("LUT 크기가 올바르지 않습니다: {n}"),
                            format!("Invalid LUT size: {n}"),
                        )
                        .into());
                    }
                    size = Some(n);
                }
                "DOMAIN_MIN" => domain.0.copy_from_slice(&numbers(3)?),
                "DOMAIN_MAX" => domain.1.copy_from_slice(&numbers(3)?),
                // 숫자로 시작하면 격자점의 색이다.
                _ if first
                    .starts_with(|c: char| c.is_ascii_digit() || matches!(c, '-' | '+' | '.')) =>
                {
                    let r: f32 = first.parse().map_err(|_| unreadable())?;
                    let rest = numbers(2)?;
                    table.push([r, rest[0], rest[1]]);
                }
                // TITLE 등 그 밖의 줄은 색에 영향을 주지 않는다.
                _ => {}
            }
        }
        let size = size.ok_or(t(
            "LUT_3D_SIZE가 없습니다. 3D LUT(.cube) 파일이 아닙니다",
            "LUT_3D_SIZE is missing. This is not a 3D LUT (.cube) file",
        ))?;
        if table.len() != size * size * size {
            let (wanted, found) = (size * size * size, table.len());
            return Err(t(
                format!("색 값이 {wanted}줄 있어야 하는데 {found}줄 있습니다"),
                format!("Expected {wanted} rows of colour values but found {found}"),
            )
            .into());
        }
        Ok(Lut {
            size,
            domain,
            table,
        })
    }

    pub fn load(path: &Path) -> Result<Lut> {
        std::fs::read_to_string(path)
            .map_err(Into::into)
            .and_then(|text| Self::parse(&text))
            .map_err(|e| {
                t(
                    format!("LUT 파일을 읽을 수 없습니다 ({}): {e}", path.display()),
                    format!("Cannot read the LUT file ({}): {e}", path.display()),
                )
                .into()
            })
    }

    /// 색 하나를 바꾼다 (사면체 보간).
    pub fn map(&self, rgb: [f32; 3]) -> [f32; 3] {
        let n = self.size;
        // 격자 좌표: 정수 부분은 칸의 위치, 소수 부분은 칸 안에서의 위치
        let position = [0, 1, 2].map(|c| {
            let (low, high) = (self.domain.0[c], self.domain.1[c]);
            ((rgb[c] - low) / (high - low)).clamp(0.0, 1.0) * (n - 1) as f32
        });
        let cell = position.map(|v| (v as usize).min(n - 2));
        let f = [0, 1, 2].map(|c| position[c] - cell[c] as f32);
        let at = |dr: usize, dg: usize, db: usize| {
            self.table[(cell[0] + dr) + (cell[1] + dg) * n + (cell[2] + db) * n * n]
        };
        // 칸을 여섯 사면체로 나눈다. 소수 부분이 큰 축부터 한 칸씩 가는 꼭짓점들을 쓴다.
        let (first, second, [w1, w2, w3]) = if f[0] >= f[1] {
            if f[1] >= f[2] {
                (at(1, 0, 0), at(1, 1, 0), [f[0], f[1], f[2]])
            } else if f[0] >= f[2] {
                (at(1, 0, 0), at(1, 0, 1), [f[0], f[2], f[1]])
            } else {
                (at(0, 0, 1), at(1, 0, 1), [f[2], f[0], f[1]])
            }
        } else if f[2] >= f[1] {
            (at(0, 0, 1), at(0, 1, 1), [f[2], f[1], f[0]])
        } else if f[2] >= f[0] {
            (at(0, 1, 0), at(0, 1, 1), [f[1], f[2], f[0]])
        } else {
            (at(0, 1, 0), at(1, 1, 0), [f[1], f[0], f[2]])
        };
        let (origin, far) = (at(0, 0, 0), at(1, 1, 1));
        [0, 1, 2].map(|k| {
            origin[k] * (1.0 - w1) + first[k] * (w1 - w2) + second[k] * (w2 - w3) + far[k] * w3
        })
    }

    /// 이미지 전체에 적용한다. `strength`가 1이면 LUT 그대로, 0이면 원본, 그 사이는 섞는다.
    pub fn apply(&self, image: &mut Image, strength: f32) {
        if strength == 0.0 {
            return;
        }
        let width = image.width;
        let [p0, p1, p2] = &mut image.planes;
        (
            p0.par_chunks_mut(width),
            p1.par_chunks_mut(width),
            p2.par_chunks_mut(width),
        )
            .into_par_iter()
            .for_each(|(r, g, b)| {
                for x in 0..width {
                    let before = [r[x], g[x], b[x]];
                    let after = self.map(before);
                    r[x] += strength * (after[0] - before[0]);
                    g[x] += strength * (after[1] - before[1]);
                    b[x] += strength * (after[2] - before[2]);
                }
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 격자점마다 `f(입력)`을 출력으로 갖는 `.cube` 글.
    fn cube(size: usize, f: impl Fn([f32; 3]) -> [f32; 3]) -> String {
        let mut text = format!("TITLE \"시험\"\n# 주석\nLUT_3D_SIZE {size}\n\n");
        let at = |i: usize| i as f32 / (size - 1) as f32;
        for b in 0..size {
            for g in 0..size {
                for r in 0..size {
                    let [x, y, z] = f([at(r), at(g), at(b)]);
                    text.push_str(&format!("{x} {y} {z}\n"));
                }
            }
        }
        text
    }

    fn near(a: [f32; 3], b: [f32; 3]) -> bool {
        a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-5)
    }

    #[test]
    fn identity_lut_leaves_colours_unchanged() {
        let lut = Lut::parse(&cube(5, |c| c)).unwrap();
        for c in [
            [0.0, 0.0, 0.0],
            [1.0, 1.0, 1.0],
            [0.13, 0.77, 0.4],
            [0.9, 0.05, 0.51],
        ] {
            assert!(near(lut.map(c), c), "{c:?} → {:?}", lut.map(c));
        }
    }

    #[test]
    fn colours_between_grid_points_are_interpolated() {
        // 격자 사이에서도 정확해야 하는 일차식: 빨강은 절반, 초록은 그대로, 파랑은 뒤집기
        let lut = Lut::parse(&cube(3, |[r, g, b]| [r * 0.5, g, 1.0 - b])).unwrap();
        for c in [
            [0.2, 0.9, 0.33],
            [0.75, 0.1, 0.6],
            [0.5, 0.5, 0.5],
            [0.31, 0.32, 0.9],
        ] {
            assert!(
                near(lut.map(c), [c[0] * 0.5, c[1], 1.0 - c[2]]),
                "{c:?} → {:?}",
                lut.map(c)
            );
        }
    }

    #[test]
    fn red_varies_fastest_in_the_file() {
        // 2×2×2: 출력의 빨강 = 입력의 빨강, 나머지는 0 → 파일의 둘째 줄이 "빨강 1"이어야 한다
        let lut =
            Lut::parse("LUT_3D_SIZE 2\n0 0 0\n1 0 0\n0 0 0\n1 0 0\n0 0 0\n1 0 0\n0 0 0\n1 0 0\n")
                .unwrap();
        assert!(near(lut.map([1.0, 0.0, 0.0]), [1.0, 0.0, 0.0]));
        assert!(near(lut.map([0.0, 1.0, 1.0]), [0.0, 0.0, 0.0]));
    }

    #[test]
    fn values_outside_the_range_are_clamped() {
        let lut = Lut::parse(&cube(3, |c| c)).unwrap();
        assert!(near(lut.map([-0.5, 1.7, 0.5]), [0.0, 1.0, 0.5]));
    }

    #[test]
    fn domain_lines_rescale_the_input() {
        let text = cube(3, |c| c).replace(
            "LUT_3D_SIZE 3\n",
            "LUT_3D_SIZE 3\nDOMAIN_MIN 0 0 0\nDOMAIN_MAX 2 2 2\n",
        );
        let lut = Lut::parse(&text).unwrap();
        // 입력 1.0은 범위(0~2)의 가운데라서 표의 가운데 값(0.5)이 나온다
        assert!(near(lut.map([1.0, 1.0, 1.0]), [0.5, 0.5, 0.5]));
    }

    #[test]
    fn broken_files_are_explained() {
        let error = |text: &str| Lut::parse(text).unwrap_err().to_string();
        assert!(
            error("0 0 0\n1 1 1\n").contains("LUT_3D_SIZE"),
            "크기가 없는 파일"
        );
        assert!(
            error("LUT_1D_SIZE 16\n").contains("1D"),
            "1D LUT는 지원하지 않는다"
        );
        let short = "LUT_3D_SIZE 2\n0 0 0\n1 1 1\n";
        assert!(
            error(short).contains("8") && error(short).contains("2"),
            "줄 수가 모자란 파일: {}",
            error(short)
        );
        let bad_number = error("LUT_3D_SIZE 2\n0 0 x\n");
        assert!(
            bad_number.contains("2번째 줄"),
            "숫자가 아닌 값은 줄 번호와 함께: {bad_number}"
        );
        assert!(error("LUT_3D_SIZE 1\n0 0 0\n").contains("크기"));
    }

    #[test]
    fn strength_blends_with_the_original_image() {
        let lut = Lut::parse(&cube(3, |[r, g, b]| [1.0 - r, 1.0 - g, 1.0 - b])).unwrap();
        let image = || Image {
            width: 2,
            height: 1,
            planes: [vec![0.2, 1.0], vec![0.4, 0.0], vec![0.6, 0.5]],
        };

        let mut full = image();
        lut.apply(&mut full, 1.0);
        assert!(near(
            [full.planes[0][0], full.planes[1][0], full.planes[2][0]],
            [0.8, 0.6, 0.4]
        ));
        assert!(near(
            [full.planes[0][1], full.planes[1][1], full.planes[2][1]],
            [0.0, 1.0, 0.5]
        ));

        let mut half = image();
        lut.apply(&mut half, 0.5);
        assert!(near(
            [half.planes[0][0], half.planes[1][0], half.planes[2][0]],
            [0.5, 0.5, 0.5]
        ));

        let mut none = image();
        lut.apply(&mut none, 0.0);
        assert_eq!(none.planes[0], [0.2, 1.0]);
    }

    #[test]
    fn loads_from_a_file_and_names_the_file_when_it_fails() {
        let dir = std::env::temp_dir().join(format!("amsil-lut-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let good = dir.join("항등.cube");
        std::fs::write(&good, cube(2, |c| c)).unwrap();
        assert!(near(
            Lut::load(&good).unwrap().map([0.3, 0.6, 0.9]),
            [0.3, 0.6, 0.9]
        ));
        let bad = dir.join("깨진.cube");
        std::fs::write(&bad, "LUT_3D_SIZE 2\n").unwrap();
        let error = Lut::load(&bad).unwrap_err().to_string();
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(error.contains("깨진.cube"), "{error}");
        // 화면에는 이 문구만 뜨므로, 무엇을 읽다가 실패했는지도 말해야 한다.
        let missing = Lut::load(&dir.join("없는.cube")).unwrap_err().to_string();
        assert!(missing.contains("없는.cube"), "{missing}");
        assert!(
            error.starts_with("LUT 파일") && missing.starts_with("LUT 파일"),
            "{error} / {missing}"
        );
    }
}
