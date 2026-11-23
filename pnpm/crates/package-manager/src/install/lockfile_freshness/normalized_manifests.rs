//! The importer manifests as the lockfile records their specifiers: with the
//! configured overrides applied.

use crate::install::{PackageManifest, Path};

/// Copies of `manifests` with `overrider` applied. `None` when it has no
/// override to apply, since the manifests as loaded are then already what the
/// lockfile records.
pub(super) fn override_normalized_manifests(
    manifests: &[(String, &PackageManifest)],
    overrider: Option<&crate::VersionsOverrider>,
) -> Option<Vec<(String, PackageManifest)>> {
    let overrider = overrider.filter(|overrider| !overrider.is_empty())?;
    let normalized = manifests
        .iter()
        .map(|(importer_id, manifest)| {
            let mut normalized = (*manifest).clone();
            let project_dir = manifest
                .path()
                .parent()
                .unwrap_or_else(|| Path::new("."));
            overrider.apply(&mut normalized, Some(project_dir));
            (importer_id.clone(), normalized)
        })
        .collect();
    Some(normalized)
}

/// The manifests a fast update compares against the lockfile: the normalized
/// ones when there are any, and `loaded` otherwise.
pub(super) fn compared_manifests<'manifest>(
    loaded: &[(String, &'manifest PackageManifest)],
    normalized: Option<&'manifest [(String, PackageManifest)]>,
) -> Vec<(String, &'manifest PackageManifest)> {
    match normalized {
        Some(normalized) => normalized
            .iter()
            .map(|(importer_id, manifest)| (importer_id.clone(), manifest))
            .collect(),
        None => loaded.to_vec(),
    }
}
