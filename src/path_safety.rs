use anyhow::{Result, ensure};
use std::path::{Component, Path};

pub(crate) const MAX_ENTRY_PATH: usize = 4096;

/// Validate portable archive names before joining them to an extraction root.
pub(crate) fn safe_path(path: &str) -> Result<()> {
    ensure!(
        !path.is_empty() && path.len() <= MAX_ENTRY_PATH,
        "invalid archive entry path length"
    );
    for part in path.split('/') {
        ensure!(
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part.ends_with(['.', ' '])
                && !part
                    .chars()
                    .any(|c| c.is_control() || "\\:<>\"|?*".contains(c)),
            "unsafe archive entry path"
        );
        let stem = part.split('.').next().unwrap_or("").to_uppercase();
        ensure!(
            !matches!(
                stem.as_str(),
                "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
            ) && !["COM", "LPT"].iter().any(|prefix| {
                stem.strip_prefix(prefix).is_some_and(|suffix| {
                    matches!(
                        suffix,
                        "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                    )
                })
            }),
            "reserved device name in archive entry"
        );
    }
    ensure!(
        Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_))),
        "unsafe archive path"
    );
    Ok(())
}
