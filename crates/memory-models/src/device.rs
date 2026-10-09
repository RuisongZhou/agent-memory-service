//! Explicit device policy: a requested Metal device never silently becomes a CPU device.
use anyhow::{bail, Context, Result};
use candle_core::{Device, DeviceLocation};
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceChoice {
    Cpu,
    Metal,
    Auto,
}

impl FromStr for DeviceChoice {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self> {
        match value {
            "cpu" => Ok(Self::Cpu),
            "metal" => Ok(Self::Metal),
            "auto" => Ok(Self::Auto),
            _ => bail!("model device must be cpu|metal|auto, got {value:?}"),
        }
    }
}

fn metal_device() -> Result<Device> {
    #[cfg(all(target_os = "macos", feature = "metal"))]
    {
        Device::new_metal(0).context("Metal device 0 is unavailable")
    }
    #[cfg(not(all(target_os = "macos", feature = "metal")))]
    {
        bail!("Metal requires macOS and a build with the memory-models metal feature")
    }
}

/// Choose once and share the returned device between embedding and reranking models.
pub fn select_device(choice: DeviceChoice) -> Result<Device> {
    match choice {
        DeviceChoice::Cpu => Ok(Device::Cpu),
        DeviceChoice::Metal => {
            metal_device().context("explicit metal request failed; CPU fallback is disabled")
        }
        DeviceChoice::Auto => match metal_device() {
            Ok(device) => Ok(device),
            Err(error) => {
                tracing::warn!(reason=%error, "auto model device: falling back to CPU");
                eprintln!("auto model device: CPU fallback ({error:#})");
                Ok(Device::Cpu)
            }
        },
    }
}

pub fn runtime_backend(device: &Device) -> &'static str {
    match device.location() {
        DeviceLocation::Metal { .. } => "candle-metal",
        DeviceLocation::Cpu if cfg!(feature = "accelerate") => "apple-accelerate",
        DeviceLocation::Cpu => "candle-gemm",
        DeviceLocation::Cuda { .. } => "unsupported-cuda",
    }
}

pub fn runtime_device(device: &Device) -> String {
    match device.location() {
        DeviceLocation::Cpu => "cpu".into(),
        DeviceLocation::Metal { gpu_id } => format!("metal:{gpu_id}"),
        DeviceLocation::Cuda { gpu_id } => format!("cuda:{gpu_id}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_only_supported_choices() {
        assert_eq!("cpu".parse::<DeviceChoice>().unwrap(), DeviceChoice::Cpu);
        assert_eq!(
            "metal".parse::<DeviceChoice>().unwrap(),
            DeviceChoice::Metal
        );
        assert_eq!("auto".parse::<DeviceChoice>().unwrap(), DeviceChoice::Auto);
        assert!("cuda".parse::<DeviceChoice>().is_err());
    }
    #[test]
    fn explicit_cpu_is_reported_as_cpu() {
        let device = select_device(DeviceChoice::Cpu).unwrap();
        assert!(device.is_cpu());
        assert_eq!(runtime_device(&device), "cpu");
        assert_ne!(runtime_backend(&device), "candle-metal");
    }
    #[cfg(not(all(target_os = "macos", feature = "metal")))]
    #[test]
    fn explicit_metal_cannot_fall_back_when_not_built() {
        assert!(select_device(DeviceChoice::Metal).is_err());
        assert!(select_device(DeviceChoice::Auto).unwrap().is_cpu());
    }
}
