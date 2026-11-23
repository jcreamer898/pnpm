//! The lockfile with the registry versions the manifests newly ask for added.

use super::FastUpdateLockfileOptions;
use crate::{
    fast_update_new_versions::{NewVersionOptions, seed_new_versions},
    install::{Arc, Lockfile, PackageManifest},
};

/// `lockfile` plus the registry versions a bump or an addition asks for that it
/// has never held, so the fast update can point the importers at them. `None`
/// when there are none to add, when the install has no registry client to look
/// them up with, or when one of them cannot be added without a resolution.
pub(super) async fn seed_never_locked_versions(
    opts: &FastUpdateLockfileOptions<'_, '_>,
    lockfile: &Lockfile,
    manifests: &[(String, &PackageManifest)],
    overrider: Option<Arc<crate::VersionsOverrider>>,
) -> Option<Lockfile> {
    let http_client = Arc::clone(opts.http_client.as_ref()?);
    let freshness = &opts.freshness;
    let hooks =
        crate::install_with_fresh_lockfile::resolver_manifest_hooks(freshness.config, overrider)
            .ok()?
            .with_pnpmfile(freshness.pnpmfile_hook.cloned());
    seed_new_versions(NewVersionOptions {
        lockfile,
        manifests,
        config: freshness.config,
        http_client,
        hooks,
        lockfile_dir: freshness.lockfile_dir,
    })
    .await
}
