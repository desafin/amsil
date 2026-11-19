//! AI 모델 실행 (ONNX Runtime). GPU(WebGPU)를 먼저 시도하고, 안 되면 CPU로 돌린다.

use crate::Result;
use crate::denoise::TILE;
use crate::lang::text;
use ort::environment::Environment;
use ort::ep;
use ort::session::Session;
use ort::value::TensorRef;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::Instant;

/// ONNX Runtime 환경은 프로세스에 하나만 만들 수 있다.
static ENV: LazyLock<std::result::Result<Environment, String>> =
    LazyLock::new(|| ort::init().build().map_err(|e| e.to_string()));

/// 모델을 실제로 돌린 시간의 누적 (나노초). 화면에 GPU 사용률을 보여 주는 데 쓴다.
static BUSY_NANOS: AtomicU64 = AtomicU64::new(0);

/// 프로그램 시작부터 지금까지 모델을 돌린 시간 (나노초).
pub fn busy_nanos() -> u64 {
    BUSY_NANOS.load(Relaxed)
}

/// CPU에서 돌리는 세션을 만든다. 작은 모델에 쓴다.
pub fn cpu_session(model: &Path) -> Result<Session> {
    let env = ENV.as_ref().map_err(|e| e.clone())?;
    Ok(Session::builder(env)?.commit_from_file(model)?)
}

/// 모델이 실제로 돌아가는 곳.
#[derive(Debug)]
pub enum Backend {
    Gpu,
    /// GPU를 쓰지 못한 이유와 함께.
    Cpu {
        reason: String,
    },
}

/// 노이즈 제거 모델 (RawNIND Bayer).
pub struct Denoiser {
    session: Session,
    pub backend: Backend,
    /// 타일 하나를 처리하는 데 걸린 시간. 읽어 들일 때 한 번 잰다.
    pub tile_seconds: f32,
}

impl Denoiser {
    /// 모델을 읽고 시험 추론까지 해 본다. GPU에서 실패하면 CPU로 다시 읽는다.
    pub fn load(model: &Path) -> Result<Self> {
        match Self::open(model, true) {
            Ok(denoiser) => Ok(denoiser),
            Err(gpu_error) => {
                let mut denoiser = Self::open(model, false)?;
                denoiser.backend = Backend::Cpu {
                    reason: gpu_error.to_string(),
                };
                Ok(denoiser)
            }
        }
    }

    fn open(model: &Path, gpu: bool) -> Result<Self> {
        let env = ENV.as_ref().map_err(|e| e.clone())?;
        let mut builder = Session::builder(env)?;
        if gpu {
            builder = builder
                .with_execution_providers([ep::WebGPU::default().build().error_on_failure()])
                .map_err(ort::Error::<()>::from)?;
        }
        let session = builder.commit_from_file(model)?;
        let mut denoiser = Self {
            session,
            backend: Backend::Gpu,
            tile_seconds: 0.0,
        };

        // 첫 실행은 준비 작업이 섞여 느리다. 동작 확인에 쓰고, 두 번째 실행으로 시간을 잰다.
        let probe = vec![0.1; 4 * TILE * TILE];
        if denoiser.infer(&probe)?.iter().any(|v| !v.is_finite()) {
            return Err(text!(
                "모델이 올바른 값을 내지 못했습니다",
                "The model produced invalid values",
            )
            .into());
        }
        let started = Instant::now();
        denoiser.infer(&probe)?;
        denoiser.tile_seconds = started.elapsed().as_secs_f32();
        Ok(denoiser)
    }

    /// 4×512×512 타일(채널 우선)을 받아 3×1024×1024를 돌려준다.
    pub fn infer(&mut self, tile: &[f32]) -> Result<Vec<f32>> {
        let started = Instant::now();
        let input = TensorRef::from_array_view(([1, 4, TILE, TILE], tile))?;
        let outputs = self.session.run(ort::inputs![input])?;
        let (_, data) = outputs[0].try_extract_tensor::<f32>()?;
        let out = data.to_vec();
        BUSY_NANOS.fetch_add(started.elapsed().as_nanos() as u64, Relaxed);
        Ok(out)
    }
}

/// 모델 폴더 안에서 노이즈 제거 모델의 위치.
pub const DENOISE_MODEL: &str = "rawdenoise-nind/model_bayer.onnx";

/// 모델을 두는 폴더. `AMSIL_MODELS`로 바꿀 수 있다.
pub fn model_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("AMSIL_MODELS") {
        return dir.into();
    }
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .unwrap_or_default();
    data.join("amsil/models")
}

/// 테스트용 모델. 저장소에 없고 (`scripts/fetch-models.sh`로 받는다) 없으면 해당 테스트는 건너뛴다.
#[cfg(test)]
pub fn test_denoiser() -> Option<Denoiser> {
    let path = model_dir().join(DENOISE_MODEL);
    if !path.exists() {
        eprintln!("노이즈 제거 모델이 없어 건너뜀");
        return None;
    }
    Some(Denoiser::load(&path).unwrap())
}

#[cfg(test)]
mod tests {
    use super::test_denoiser as denoiser;
    use crate::denoise::{self, Packed, TILE};

    #[test]
    fn model_turns_a_packed_tile_into_a_double_size_rgb_tile() {
        let Some(mut model) = denoiser() else { return };
        let out = model.infer(&vec![0.2; 4 * TILE * TILE]).unwrap();
        assert_eq!(out.len(), 3 * (TILE * 2) * (TILE * 2));
        assert!(out.iter().all(|v| v.is_finite()));
        assert!(model.tile_seconds > 0.0);
        eprintln!("{:?}, 타일당 {:.3}초", model.backend, model.tile_seconds);
    }

    /// 재현 가능한 정규분포 난수 (Box-Muller).
    fn noise(seed: &mut u64) -> f32 {
        let mut uniform = || {
            *seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((*seed >> 40) as f32 + 0.5) / (1u64 << 24) as f32
        };
        (-2.0 * uniform().ln()).sqrt() * (std::f32::consts::TAU * uniform()).cos()
    }

    #[test]
    fn denoising_a_noisy_flat_patch_keeps_its_colour_and_removes_most_noise() {
        let Some(mut model) = denoiser() else { return };
        // 화이트밸런스 전의 균일한 색(R 0.10, G 0.20, B 0.15)에 표준편차 0.02의 노이즈
        let (w, h, sigma) = (512, 512, 0.02);
        let mut seed = 1;
        let mut plane = |level: f32| {
            (0..w * h)
                .map(|_| (level + sigma * noise(&mut seed)).clamp(0.0, 1.0))
                .collect()
        };
        let packed = Packed {
            width: w,
            height: h,
            planes: [plane(0.10), plane(0.20), plane(0.20), plane(0.15)],
        };

        let out = denoise::run(&packed, |tile| model.infer(tile)).unwrap();

        assert_eq!((out.width, out.height), (w * 2, h * 2));
        for (c, level) in [0.10f32, 0.20, 0.15].into_iter().enumerate() {
            let n = out.planes[c].len() as f32;
            let mean = out.planes[c].iter().sum::<f32>() / n;
            let std = (out.planes[c]
                .iter()
                .map(|v| (v - mean).powi(2))
                .sum::<f32>()
                / n)
                .sqrt();
            assert!(
                (mean - level).abs() < 0.03 * level,
                "채널 {c}: 평균 {mean} (기대 {level})"
            );
            assert!(std < sigma / 4.0, "채널 {c}: 남은 노이즈 {std}");
        }
    }
}
