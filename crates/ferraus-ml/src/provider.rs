//! Execution-provider selection: try `cuda`, `coreml`, `directml`, `cpu` in that order unless
//! `--device` pins one; a provider that fails to initialize logs a warning and the next one
//! (ultimately CPU) is used. The selection logic is pure so it can be unit-tested with a
//! simulated failure.

use ferraus_core::{Error, Result};
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Provider {
    Cuda,
    CoreMl,
    DirectMl,
    Cpu,
}

/// The automatic order from the PRD.
pub const AUTO_ORDER: [Provider; 4] = [
    Provider::Cuda,
    Provider::CoreMl,
    Provider::DirectMl,
    Provider::Cpu,
];

impl Provider {
    pub fn name(self) -> &'static str {
        match self {
            Provider::Cuda => "cuda",
            Provider::CoreMl => "coreml",
            Provider::DirectMl => "directml",
            Provider::Cpu => "cpu",
        }
    }

    /// Parse `--device`; `auto` → `None`.
    pub fn parse(s: &str) -> Result<Option<Provider>> {
        Ok(match s.trim().to_ascii_lowercase().as_str() {
            "" | "auto" => None,
            "cpu" => Some(Provider::Cpu),
            "cuda" | "gpu" => Some(Provider::Cuda),
            "coreml" | "metal" => Some(Provider::CoreMl),
            "directml" | "dml" => Some(Provider::DirectMl),
            other => {
                return Err(Error::usage(format!(
                    "--device: '{other}' is not one of auto, cpu, cuda, coreml, directml"
                )))
            }
        })
    }

    /// Whether this build of ferraus contains the provider's registration code.
    pub fn compiled(self) -> bool {
        match self {
            Provider::Cpu => true,
            Provider::Cuda => cfg!(feature = "cuda"),
            Provider::CoreMl => cfg!(any(feature = "coreml", target_os = "macos")),
            Provider::DirectMl => cfg!(any(feature = "directml", windows)),
        }
    }
}

/// Providers to try, in order. Automatic mode skips providers this build does not contain;
/// a pinned provider is tried even when not compiled (so the user gets a clear warning) and
/// always falls back to CPU. Deterministic mode in automatic selection uses CPU only, since
/// bit-exact reproducibility is promised on CPU.
pub fn candidates(device: &str, deterministic: bool, compiled: &dyn Fn(Provider) -> bool) -> Result<Vec<Provider>> {
    Ok(match Provider::parse(device)? {
        None if deterministic => vec![Provider::Cpu],
        None => AUTO_ORDER.iter().copied().filter(|p| compiled(*p)).collect(),
        Some(Provider::Cpu) => vec![Provider::Cpu],
        Some(p) => vec![p, Provider::Cpu],
    })
}

/// Result of a selection: the initialized value, which provider produced it, and the
/// warnings to surface (one per provider that failed).
#[derive(Debug)]
pub struct Selected<T> {
    pub value: T,
    pub provider: Provider,
    pub warnings: Vec<String>,
}

/// Try each provider in `order` with `init`; the first success wins. Failures become
/// warnings ("cuda failed to initialize (...); falling back to cpu").
pub fn select<T>(
    order: &[Provider],
    mut init: impl FnMut(Provider) -> std::result::Result<T, String>,
) -> Result<Selected<T>> {
    let mut warnings = Vec::new();
    let mut last = String::new();
    for (i, p) in order.iter().enumerate() {
        match init(*p) {
            Ok(value) => {
                return Ok(Selected {
                    value,
                    provider: *p,
                    warnings,
                })
            }
            Err(e) => {
                let next = order.get(i + 1).map(|n| n.name()).unwrap_or("nothing");
                warnings.push(format!(
                    "{} execution provider failed to initialize ({e}); falling back to {next}",
                    p.name()
                ));
                last = e;
            }
        }
    }
    Err(Error::processing(format!(
        "no execution provider could load the model: {last}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all(_: Provider) -> bool {
        true
    }

    #[test]
    fn auto_order_and_pinning() {
        assert_eq!(candidates("auto", false, &all).unwrap(), AUTO_ORDER.to_vec());
        assert_eq!(
            candidates("auto", false, &|p| p == Provider::Cpu).unwrap(),
            vec![Provider::Cpu]
        );
        assert_eq!(
            candidates("cuda", false, &|p| p == Provider::Cpu).unwrap(),
            vec![Provider::Cuda, Provider::Cpu]
        );
        assert_eq!(candidates("cpu", false, &all).unwrap(), vec![Provider::Cpu]);
        assert_eq!(candidates("auto", true, &all).unwrap(), vec![Provider::Cpu]);
        assert!(candidates("tpu", false, &all).is_err());
    }

    #[test]
    fn failing_provider_falls_back_with_warning() {
        let order = candidates("auto", false, &all).unwrap();
        let mut tried = Vec::new();
        let sel = select(&order, |p| {
            tried.push(p);
            match p {
                Provider::Cuda => Err("libcudart.so.12: cannot open shared object file".into()),
                Provider::CoreMl => Err("CoreML not supported".into()),
                Provider::DirectMl => Err("no adapter".into()),
                Provider::Cpu => Ok(42),
            }
        })
        .unwrap();
        assert_eq!(sel.value, 42);
        assert_eq!(sel.provider, Provider::Cpu);
        assert_eq!(tried, AUTO_ORDER.to_vec());
        assert_eq!(sel.warnings.len(), 3);
        assert!(sel.warnings[0].contains("cuda execution provider failed"));
        assert!(sel.warnings[0].contains("falling back to coreml"));
        assert!(sel.warnings[2].contains("falling back to cpu"));
    }

    #[test]
    fn pinned_failure_warns_then_cpu() {
        let order = candidates("cuda", false, &|_| false).unwrap();
        let sel = select(&order, |p| {
            if p == Provider::Cuda {
                Err("not compiled".to_string())
            } else {
                Ok(p)
            }
        })
        .unwrap();
        assert_eq!(sel.provider, Provider::Cpu);
        assert_eq!(sel.warnings.len(), 1);
    }

    #[test]
    fn first_success_stops() {
        let order = AUTO_ORDER.to_vec();
        let sel = select(&order, |p| if p == Provider::CoreMl { Ok(1) } else { Err("x".into()) }).unwrap();
        assert_eq!(sel.provider, Provider::CoreMl);
        assert_eq!(sel.warnings.len(), 1);
    }

    #[test]
    fn total_failure_is_an_error() {
        assert!(select::<()>(&[Provider::Cpu], |_| Err("broken model".into())).is_err());
    }
}
