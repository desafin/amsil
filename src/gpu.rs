//! Runs AI models (ONNX Runtime). Tries the GPU (WebGPU) first and falls back to the CPU.

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

/// Only one ONNX Runtime environment can be created per process.
static ENV: LazyLock<std::result::Result<Environment, String>> =
    LazyLock::new(|| ort::init().build().map_err(|e| e.to_string()));

/// Accumulated time spent actually running models (nanoseconds). Used to show GPU usage on screen.
static BUSY_NANOS: AtomicU64 = AtomicU64::new(0);

/// Time spent running models since program start (nanoseconds).
pub fn busy_nanos() -> u64 {
    BUSY_NANOS.load(Relaxed)
}

/// Creates a session that runs on the CPU. Used for small models.
pub fn cpu_session(model: &Path) -> Result<Session> {
    let env = ENV.as_ref().map_err(|e| e.clone())?;
    Ok(Session::builder(env)?.commit_from_file(model)?)
}

/// Where the model actually runs.
#[derive(Debug)]
pub enum Backend {
    Gpu,
    /// Along with the reason the GPU could not be used.
    Cpu {
        reason: String,
    },
}

/// Denoising model (RawNIND Bayer).
pub struct Denoiser {
    session: Session,
    pub backend: Backend,
    /// Time taken to process one tile. Measured once at load time.
    pub tile_seconds: f32,
}

impl Denoiser {
    /// Loads the model and runs a trial inference. If the GPU fails, reloads it on the CPU.
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

        // The first run is slow because it includes warm-up work. Use it to check that the model
        // works, and time the second run.
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

    /// Takes a 4×512×512 tile (channel-first) and returns 3×1024×1024.
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

/// Location of the denoising model inside the model folder.
pub const DENOISE_MODEL: &str = "rawdenoise-nind/model_bayer.onnx";

/// Folder that holds the models. Can be changed with `AMSIL_MODELS`.
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

/// Model for tests. Not in the repository (fetch it with `scripts/fetch-models.sh`); tests that
/// need it are skipped when it is missing.
#[cfg(test)]
pub fn test_denoiser() -> Option<Denoiser> {
    let path = model_dir().join(DENOISE_MODEL);
    if !path.exists() {
        eprintln!("denoising model not found, skipping");
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
        eprintln!("{:?}, {:.3} s per tile", model.backend, model.tile_seconds);
    }

    /// Reproducible normally distributed random numbers (Box-Muller).
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
        // A uniform color before white balance (R 0.10, G 0.20, B 0.15) plus noise with a standard
        // deviation of 0.02
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
                "channel {c}: mean {mean} (expected {level})"
            );
            assert!(std < sigma / 4.0, "channel {c}: remaining noise {std}");
        }
    }
}
