//! 보정 값(Params)과 그것을 화소에 적용하는 연산.
//!
//! 모든 값은 0이 "변화 없음"이다. 그래서 프리셋에 강도를 곱하고 수동 조정을 더하는 것만으로 조합된다.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Params {
    /// 노출 (EV).
    #[serde(skip_serializing_if = "is_zero")]
    pub exposure: f32,
    /// 이하 -1 ~ 1.
    #[serde(skip_serializing_if = "is_zero")]
    pub contrast: f32,
    #[serde(skip_serializing_if = "is_zero")]
    pub highlights: f32,
    #[serde(skip_serializing_if = "is_zero")]
    pub shadows: f32,
    /// 색온도: +는 따뜻하게, -는 차갑게.
    #[serde(skip_serializing_if = "is_zero")]
    pub temp: f32,
    /// 틴트: +는 자홍, -는 녹색.
    #[serde(skip_serializing_if = "is_zero")]
    pub tint: f32,
    #[serde(skip_serializing_if = "is_zero")]
    pub saturation: f32,
    /// 생동감: 채도가 낮은 색을 더 많이 올린다.
    #[serde(skip_serializing_if = "is_zero")]
    pub vibrance: f32,
    /// 선명도: 넓은 범위의 대비.
    #[serde(skip_serializing_if = "is_zero")]
    pub clarity: f32,
    /// 샤픈: 가는 윤곽.
    #[serde(skip_serializing_if = "is_zero")]
    pub sharpen: f32,
    /// 비네팅: +는 가장자리를 어둡게, -는 밝게.
    #[serde(skip_serializing_if = "is_zero")]
    pub vignette: f32,
    /// 흑백 정도 (0 ~ 1).
    #[serde(skip_serializing_if = "is_zero")]
    pub mono: f32,
    /// 색상대별 채도와 밝기.
    #[serde(skip_serializing_if = "Hsl::is_default")]
    pub hsl: Hsl,
}

/// 색상대별 조정.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Hsl {
    #[serde(skip_serializing_if = "Band::is_default")]
    pub red: Band,
    #[serde(skip_serializing_if = "Band::is_default")]
    pub orange: Band,
    #[serde(skip_serializing_if = "Band::is_default")]
    pub yellow: Band,
    #[serde(skip_serializing_if = "Band::is_default")]
    pub green: Band,
    #[serde(skip_serializing_if = "Band::is_default")]
    pub aqua: Band,
    #[serde(skip_serializing_if = "Band::is_default")]
    pub blue: Band,
    #[serde(skip_serializing_if = "Band::is_default")]
    pub purple: Band,
    #[serde(skip_serializing_if = "Band::is_default")]
    pub magenta: Band,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Band {
    #[serde(skip_serializing_if = "is_zero")]
    pub sat: f32,
    #[serde(skip_serializing_if = "is_zero")]
    pub lum: f32,
}

impl Params {
    /// 모든 값에 `k`를 곱한다 (프리셋의 강도).
    pub fn scaled(&self, k: f32) -> Params {
        self.zip(self, |value, _| value * k)
    }

    /// 두 보정을 더한다 (프리셋 + 수동 조정).
    pub fn plus(&self, other: &Params) -> Params {
        self.zip(other, |a, b| a + b)
    }

    /// 아무것도 바꾸지 않는 값인가.
    pub fn is_neutral(&self) -> bool {
        *self == Params::default()
    }

    /// 색을 다루는 값(화소 단위로 적용되는 것)이 하나라도 있는가.
    pub fn changes_colour(&self) -> bool {
        let tone = [
            self.contrast,
            self.highlights,
            self.shadows,
            self.saturation,
            self.vibrance,
            self.mono,
        ];
        tone.iter().any(|&v| v != 0.0) || self.hsl != Hsl::default()
    }

    /// 두 보정의 같은 항목끼리 `f`를 적용한다.
    fn zip(&self, other: &Params, f: impl Fn(f32, f32) -> f32) -> Params {
        let band = |a: Band, b: Band| Band {
            sat: f(a.sat, b.sat),
            lum: f(a.lum, b.lum),
        };
        let (h, o) = (&self.hsl, &other.hsl);
        Params {
            exposure: f(self.exposure, other.exposure),
            contrast: f(self.contrast, other.contrast),
            highlights: f(self.highlights, other.highlights),
            shadows: f(self.shadows, other.shadows),
            temp: f(self.temp, other.temp),
            tint: f(self.tint, other.tint),
            saturation: f(self.saturation, other.saturation),
            vibrance: f(self.vibrance, other.vibrance),
            clarity: f(self.clarity, other.clarity),
            sharpen: f(self.sharpen, other.sharpen),
            vignette: f(self.vignette, other.vignette),
            mono: f(self.mono, other.mono),
            hsl: Hsl {
                red: band(h.red, o.red),
                orange: band(h.orange, o.orange),
                yellow: band(h.yellow, o.yellow),
                green: band(h.green, o.green),
                aqua: band(h.aqua, o.aqua),
                blue: band(h.blue, o.blue),
                purple: band(h.purple, o.purple),
                magenta: band(h.magenta, o.magenta),
            },
        }
    }
}

fn is_zero(value: &f32) -> bool {
    *value == 0.0
}

impl Band {
    fn is_default(&self) -> bool {
        *self == Band::default()
    }
}

impl Hsl {
    fn is_default(&self) -> bool {
        *self == Hsl::default()
    }

    fn bands(&self) -> [Band; 8] {
        [
            self.red,
            self.orange,
            self.yellow,
            self.green,
            self.aqua,
            self.blue,
            self.purple,
            self.magenta,
        ]
    }
}

/// 선형 sRGB → Oklab.
pub fn to_oklab([r, g, b]: [f32; 3]) -> [f32; 3] {
    let l = (0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ]
}

/// Oklab → 선형 sRGB.
pub fn from_oklab([l, a, b]: [f32; 3]) -> [f32; 3] {
    let cube = |v: f32| v * v * v;
    let l_ = cube(l + 0.396_337_78 * a + 0.215_803_76 * b);
    let m_ = cube(l - 0.105_561_346 * a - 0.063_854_17 * b);
    let s_ = cube(l - 0.089_484_18 * a - 1.291_485_5 * b);
    [
        4.076_741_7 * l_ - 3.307_711_6 * m_ + 0.230_969_94 * s_,
        -1.268_438 * l_ + 2.609_757_4 * m_ - 0.341_319_38 * s_,
        -0.004_196_086_3 * l_ - 0.703_418_6 * m_ + 1.707_614_7 * s_,
    ]
}

/// 대비 슬라이더가 끝까지 갔을 때의 세기. 1을 넘으면 밝기 순서가 뒤집힐 수 있다.
const CONTRAST_GAIN: f32 = 0.9;
/// 이 정도 채도면 이미 선명한 색으로 본다 (Oklab 채도).
const VIVID: f32 = 0.25;
/// 색상대의 중심 (Oklab 색상각, 도). `Hsl`의 항목과 같은 순서.
const HUE_CENTRES: [f32; 8] = [29.0, 58.0, 110.0, 142.0, 195.0, 264.0, 303.0, 328.0];
/// 색상대별 밝기 조정이 끝까지 갔을 때의 세기.
const HUE_LUM_GAIN: f32 = 0.4;

/// 화소 하나의 색 보정 (대비, 하이라이트/그림자, 채도, 생동감, 색상대별, 흑백). Oklab에서 한다.
pub fn adjust([l, a, b]: [f32; 3], p: &Params) -> [f32; 3] {
    use std::f32::consts::TAU;
    // 밝기: 각 식은 0과 1을 고정하고 밝기 순서를 뒤집지 않는다.
    let mut l = l.clamp(0.0, 1.0);
    l += p.shadows * l * (1.0 - l).powi(3);
    l += p.highlights * l.powi(3) * (1.0 - l);
    l -= p.contrast * CONTRAST_GAIN * (TAU * l).sin() / TAU;

    let chroma = a.hypot(b);
    if chroma == 0.0 {
        return [l, a, b];
    }
    let mut gain = (1.0 + p.saturation).max(0.0);
    gain *= (1.0 + p.vibrance * (1.0 - (chroma / VIVID).min(1.0))).max(0.0);
    if p.hsl != Hsl::default() {
        let hue = b.atan2(a).to_degrees().rem_euclid(360.0);
        let (sat, lum) = hue_adjustment(hue, &p.hsl.bands());
        gain *= (1.0 + sat).max(0.0);
        // 거의 무채색인 화소는 색상이 불안정하므로 밝기 조정을 약하게 건다.
        l = (l + lum * HUE_LUM_GAIN * (chroma / 0.04).min(1.0)).clamp(0.0, 1.0);
    }
    gain *= 1.0 - p.mono.clamp(0.0, 1.0);
    [l, a * gain, b * gain]
}

/// 색상각에 해당하는 (채도, 밝기) 조정. 이웃한 두 색상대 사이에서는 부드럽게 섞는다.
fn hue_adjustment(hue: f32, bands: &[Band; 8]) -> (f32, f32) {
    for i in 0..8 {
        let next = (i + 1) % 8;
        let (from, mut to) = (HUE_CENTRES[i], HUE_CENTRES[next]);
        let mut h = hue;
        if to < from {
            // 마지막 구간은 360도를 넘어 첫 색상대로 이어진다.
            to += 360.0;
            if h < from {
                h += 360.0;
            }
        }
        if (from..to).contains(&h) {
            let t = (h - from) / (to - from);
            return (
                bands[i].sat * (1.0 - t) + bands[next].sat * t,
                bands[i].lum * (1.0 - t) + bands[next].lum * t,
            );
        }
    }
    (0.0, 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chroma(lab: [f32; 3]) -> f32 {
        lab[1].hypot(lab[2])
    }

    fn near(a: [f32; 3], b: [f32; 3], tolerance: f32) -> bool {
        a.iter().zip(&b).all(|(x, y)| (x - y).abs() < tolerance)
    }

    #[test]
    fn oklab_round_trips() {
        for rgb in [
            [0.0, 0.0, 0.0],
            [1.0, 1.0, 1.0],
            [0.5, 0.2, 0.05],
            [0.02, 0.3, 0.9],
        ] {
            assert!(near(from_oklab(to_oklab(rgb)), rgb, 1e-4), "{rgb:?}");
        }
        let white = to_oklab([1.0; 3]);
        assert!(
            near(white, [1.0, 0.0, 0.0], 1e-3),
            "흰색은 L=1, 무채색: {white:?}"
        );
    }

    #[test]
    fn scaling_and_adding_combine_a_preset_with_manual_adjustments() {
        let mut preset = Params {
            exposure: 1.0,
            saturation: 0.4,
            ..Params::default()
        };
        preset.hsl.orange.sat = 0.2;
        let manual = Params {
            exposure: -0.25,
            contrast: 0.1,
            ..Params::default()
        };
        let total = preset.scaled(0.5).plus(&manual);
        assert_eq!(
            (total.exposure, total.saturation, total.contrast),
            (0.25, 0.2, 0.1)
        );
        assert_eq!(total.hsl.orange.sat, 0.1);
        assert!(Params::default().is_neutral());
        assert!(!total.is_neutral());
        assert!(preset.scaled(0.0).is_neutral(), "강도 0이면 효과가 없다");
    }

    #[test]
    fn neutral_params_change_nothing() {
        for lab in [[0.2, 0.05, -0.1], [0.8, -0.1, 0.12], [0.5, 0.0, 0.0]] {
            assert_eq!(adjust(lab, &Params::default()), lab);
        }
    }

    #[test]
    fn saturation_and_mono_remove_colour() {
        let lab = to_oklab([0.6, 0.2, 0.1]);
        let grey = adjust(
            lab,
            &Params {
                saturation: -1.0,
                ..Params::default()
            },
        );
        assert!(chroma(grey) < 1e-6);
        assert_eq!(grey[0], lab[0], "밝기는 그대로");
        assert!(
            chroma(adjust(
                lab,
                &Params {
                    mono: 1.0,
                    ..Params::default()
                }
            )) < 1e-6
        );
        let more = adjust(
            lab,
            &Params {
                saturation: 0.5,
                ..Params::default()
            },
        );
        assert!((chroma(more) / chroma(lab) - 1.5).abs() < 1e-4);
    }

    #[test]
    fn vibrance_lifts_dull_colours_more_than_vivid_ones() {
        let p = Params {
            vibrance: 1.0,
            ..Params::default()
        };
        let (dull, vivid) = ([0.6, 0.02, 0.01], [0.6, 0.2, 0.1]);
        let gain = |lab: [f32; 3]| chroma(adjust(lab, &p)) / chroma(lab);
        assert!(gain(dull) > 1.5, "{}", gain(dull));
        assert!(
            gain(vivid) < gain(dull) && gain(vivid) >= 1.0,
            "{}",
            gain(vivid)
        );
    }

    #[test]
    fn contrast_darkens_darks_and_brightens_brights() {
        let p = Params {
            contrast: 0.8,
            ..Params::default()
        };
        let l = |v: f32| adjust([v, 0.0, 0.0], &p)[0];
        assert!(l(0.25) < 0.25 && l(0.75) > 0.75);
        assert!((l(0.5) - 0.5).abs() < 1e-6, "중간은 그대로");
        assert!(
            l(0.0).abs() < 1e-6 && (l(1.0) - 1.0).abs() < 1e-6,
            "양 끝은 그대로"
        );
    }

    #[test]
    fn shadows_lift_the_dark_end_and_highlights_pull_the_bright_end() {
        let l = |v: f32, p: Params| adjust([v, 0.0, 0.0], &p)[0];
        let shadows = Params {
            shadows: 1.0,
            ..Params::default()
        };
        assert!(l(0.2, shadows) > 0.25, "{}", l(0.2, shadows));
        assert!(
            (l(0.95, shadows) - 0.95).abs() < 0.01,
            "밝은 곳은 거의 그대로"
        );
        let highlights = Params {
            highlights: -1.0,
            ..Params::default()
        };
        assert!(l(0.8, highlights) < 0.75, "{}", l(0.8, highlights));
        assert!(
            (l(0.1, highlights) - 0.1).abs() < 0.01,
            "어두운 곳은 거의 그대로"
        );
    }

    #[test]
    fn brightness_order_is_kept_even_at_extreme_settings() {
        for sign in [-1.0, 1.0] {
            let p = Params {
                contrast: sign,
                highlights: sign,
                shadows: -sign,
                ..Params::default()
            };
            let levels: Vec<f32> = (0..=100)
                .map(|i| adjust([i as f32 / 100.0, 0.0, 0.0], &p)[0])
                .collect();
            assert!(
                levels.windows(2).all(|w| w[0] <= w[1]),
                "{sign}: {levels:?}"
            );
            assert!(levels.iter().all(|v| (0.0..=1.0).contains(v)));
        }
    }

    #[test]
    fn a_hue_band_changes_only_colours_near_it() {
        let mut p = Params::default();
        p.hsl.orange.sat = 0.5;
        p.hsl.blue.lum = -0.3;
        let orange = to_oklab([0.8, 0.3, 0.02]);
        let blue = to_oklab([0.02, 0.1, 0.8]);
        let green = to_oklab([0.1, 0.6, 0.1]);
        assert!(
            chroma(adjust(orange, &p)) > chroma(orange) * 1.3,
            "주황의 채도가 오른다"
        );
        assert!(adjust(blue, &p)[0] < blue[0] - 0.03, "파랑이 어두워진다");
        assert!(
            (chroma(adjust(blue, &p)) - chroma(blue)).abs() < 1e-3,
            "파랑의 채도는 그대로"
        );
        assert!(near(adjust(green, &p), green, 1e-3), "초록은 그대로");
        assert_eq!(
            adjust([0.5, 0.0, 0.0], &p),
            [0.5, 0.0, 0.0],
            "무채색은 색상이 없으니 그대로"
        );
    }

    #[test]
    fn params_read_from_toml_with_missing_fields_as_zero() {
        let p: Params = toml::from_str("contrast = 0.2\n[hsl]\norange = { sat = 0.15 }\n").unwrap();
        assert_eq!(
            (p.contrast, p.hsl.orange.sat, p.exposure, p.hsl.blue.lum),
            (0.2, 0.15, 0.0, 0.0)
        );
        assert!(
            toml::from_str::<Params>("contrsat = 0.2\n").is_err(),
            "오타는 조용히 넘어가지 않는다"
        );
    }
}
