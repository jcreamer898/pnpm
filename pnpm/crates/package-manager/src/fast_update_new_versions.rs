use crate::{
    fast_update_importers::{importer_dependency, locked_version_resolution_would_pick},
    fast_update_overrides::{
        RewritePlan, effective_dependencies, get_peer_names, is_safe_registry_result,
        manifest_dependency_map, package_metadata, validate_dependencies,
    },
    resolution_policy::{PickPolicy, create_configured_registry_resolver},
};
use futures_util::future::join_all;
use pnpm_config::{Config, TrustPolicy};
use pnpm_hooks::HookContext;
use pnpm_lockfile::{
    Lockfile, LockfileFormOptions, LockfileResolution, PackageKey, PackageMetadata, PkgName,
    ProjectSnapshot, SnapshotEntry, pick_registry_for_package, registry_server_type,
};
use pnpm_network::ThrottledClient;
use pnpm_package_manifest::{DependencyGroup, PackageManifest};
use pnpm_resolving_default_resolver::DefaultResolver;
use pnpm_resolving_deps_resolver::ManifestTransformHooks;
use pnpm_resolving_resolver_base::{
    ResolutionPolicyOptions, ResolveOptions, Resolver, ResolverProjectOptions,
    VersionSelectionOptions, WantedDependency, parse_strict_semver_range,
};
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::Arc,
};

/// What [`seed_new_versions`] reads and the registry lookups it makes go
/// through.
pub(crate) struct NewVersionOptions<'a> {
    pub lockfile: &'a Lockfile,
    pub manifests: &'a [(String, &'a PackageManifest)],
    pub config: &'a Config,
    pub http_client: Arc<ThrottledClient>,
    pub hooks: ManifestTransformHooks,
    pub lockfile_dir: &'a Path,
}

/// A registry version a manifest asks for that the lockfile has never held.
#[derive(Debug, PartialEq)]
struct NewVersionRequest {
    name: PkgName,
    specifier: String,
}

/// The version the registry resolver picked for a request, with its manifest
/// as every resolution reads it: through the package extensions, the
/// pnpmfile's `readPackage` and the overrides.
struct ResolvedVersion {
    name: PkgName,
    version: String,
    manifest: Arc<Value>,
    resolution: LockfileResolution,
}

/// The records the lockfile needs to hold a resolved version.
struct LockedEntry {
    key: PackageKey,
    metadata: PackageMetadata,
    snapshot: SnapshotEntry,
}

/// Add the registry versions that dependency bumps and additions ask for to a
/// copy of the lockfile, so the fast-update compose can point the importers at
/// them instead of sending the whole workspace through a full resolution and
/// peer-dependency pass.
///
/// A version is only added when installing it is provably local: its manifest
/// declares no peers and none of the other things the compose refuses (see
/// [`is_safe_registry_result`]), and every dependency it declares is satisfied
/// by exactly one non-optional version the lockfile already holds. The old
/// version stays in place, because other importers may still use it; pruning
/// the result drops it when nothing does.
///
/// `None` when there is nothing to add, or when anything about it is not
/// provably local. The caller then compiles the lockfile as it did before.
pub(crate) async fn seed_new_versions(opts: NewVersionOptions<'_>) -> Option<Lockfile> {
    let requests = requested_versions(opts.lockfile, opts.manifests, opts.config)?;
    let resolved = resolve_requests(&opts, &requests).await?;
    add_resolved_versions(opts.lockfile, opts.config, &resolved)
}

/// The versions the manifests ask for that the lockfile has never held.
///
/// `None` when there are none, or when the lockfile or the settings hold
/// something a seeded version would contradict: a `time` map records a publish
/// date per direct dependency, and a trust policy compares each version's
/// publish history. Also `None` when a request names a workspace project,
/// which resolves to a directory rather than to the registry.
fn requested_versions(
    lockfile: &Lockfile,
    manifests: &[(String, &PackageManifest)],
    config: &Config,
) -> Option<Vec<NewVersionRequest>> {
    if lockfile.time.is_some() || config.trust_policy != TrustPolicy::Off {
        return None;
    }
    let requests = new_version_requests(lockfile, manifests);
    if requests.is_empty() || asks_for_workspace_package(&requests, manifests) {
        return None;
    }
    Some(requests)
}

/// The dependencies whose declared range no locked version satisfies, once
/// each. A range with a locked match is the compose's own business.
fn new_version_requests(
    lockfile: &Lockfile,
    manifests: &[(String, &PackageManifest)],
) -> Vec<NewVersionRequest> {
    let mut requests = Vec::new();
    for (importer_id, manifest) in manifests {
        for request in unlocked_requests(lockfile, importer_id, manifest) {
            if !requests.contains(&request) {
                requests.push(request);
            }
        }
    }
    requests
}

/// The requests one project's manifest makes.
///
/// A name declared under `optionalDependencies` is left to the full
/// resolution, even when it is also declared as a regular dependency: a
/// seeded version cannot tell on its own whether only optional edges reach it.
fn unlocked_requests(
    lockfile: &Lockfile,
    importer_id: &str,
    manifest: &PackageManifest,
) -> Vec<NewVersionRequest> {
    let importer = lockfile.importers.get(importer_id);
    let optional: HashSet<&str> = manifest
        .dependencies([DependencyGroup::Optional])
        .map(|(name, _)| name)
        .collect();
    manifest
        .dependencies([DependencyGroup::Dev, DependencyGroup::Prod])
        .filter(|(name, _)| !optional.contains(name))
        .filter_map(|(name, specifier)| unlocked_request(lockfile, importer, name, specifier))
        .collect()
}

/// The request for one declared dependency, when it asks for a registry
/// version the lockfile does not hold. A declaration the importer already
/// records, and a specifier that is not a plain version range, are not.
fn unlocked_request(
    lockfile: &Lockfile,
    importer: Option<&ProjectSnapshot>,
    name: &str,
    specifier: &str,
) -> Option<NewVersionRequest> {
    let alias = PkgName::parse(name).ok()?;
    let recorded = importer.and_then(|importer| importer_dependency(importer, &alias));
    if recorded.is_some_and(|(_, spec)| spec.specifier == specifier) {
        return None;
    }
    let range = parse_strict_semver_range(specifier)?;
    let snapshots = lockfile.snapshots.as_ref();
    if locked_version_resolution_would_pick(snapshots, &alias, &range, false).is_some() {
        return None;
    }
    Some(NewVersionRequest { name: alias, specifier: specifier.to_string() })
}

/// Whether a request names a workspace project, which resolves to a directory
/// rather than to a registry version.
fn asks_for_workspace_package(
    requests: &[NewVersionRequest],
    manifests: &[(String, &PackageManifest)],
) -> bool {
    let workspace_names: HashSet<&str> = manifests
        .iter()
        .filter_map(|(_, manifest)| manifest.value().get("name")?.as_str())
        .collect();
    requests
        .iter()
        .any(|request| workspace_names.contains(request.name.to_string().as_str()))
}

/// The version the registry resolver picks for each request, concurrently.
/// `None` when the resolution mode picks something other than the highest
/// version, or when any request resolves to something other than a plain
/// registry version.
async fn resolve_requests(
    opts: &NewVersionOptions<'_>,
    requests: &[NewVersionRequest],
) -> Option<Vec<ResolvedVersion>> {
    let policy = PickPolicy::from_config(opts.config).ok()?;
    if policy.time_based || policy.pick_lowest_direct {
        return None;
    }
    let resolver = single_attempt_resolver(opts, &policy)?;
    let resolve_options = resolver_options(opts, &policy);
    join_all(
        requests
            .iter()
            .map(|request| resolve_request(&resolver, &resolve_options, &opts.hooks, request)),
    )
    .await
    .into_iter()
    .collect()
}

/// A registry resolver that makes one attempt per lookup. A failed lookup sends
/// the install to the full resolution, which retries as configured, so
/// retrying here as well would only double the wait when the registry is down.
///
/// This is a plain function so the copy of the settings it edits is gone
/// before the lookups start, instead of being held in their future.
fn single_attempt_resolver(
    opts: &NewVersionOptions<'_>,
    policy: &PickPolicy,
) -> Option<DefaultResolver> {
    let mut config = opts.config.clone();
    config.fetch_retries = 0;
    create_configured_registry_resolver(&config, Arc::clone(&opts.http_client), policy).ok()
}

fn resolver_options(opts: &NewVersionOptions<'_>, policy: &PickPolicy) -> ResolveOptions {
    ResolveOptions {
        project: ResolverProjectOptions {
            project_dir: opts.lockfile_dir.to_path_buf(),
            lockfile_dir: opts.lockfile_dir.to_path_buf(),
            workspace_dir: opts.config.workspace_dir.clone(),
            ..ResolverProjectOptions::default()
        },
        version: VersionSelectionOptions {
            default_tag: Some(opts.config.default_tag().to_string()),
            ..VersionSelectionOptions::default()
        },
        policy: ResolutionPolicyOptions {
            published_by: policy.published_by,
            published_by_exclude: policy.published_by_exclude.clone(),
            ..ResolutionPolicyOptions::default()
        },
        ..ResolveOptions::default()
    }
}

/// The version the registry resolver picks for one request. `None` when it
/// finds nothing, or finds something [`is_safe_registry_result`] refuses.
async fn resolve_request(
    resolver: &dyn Resolver,
    resolve_options: &ResolveOptions,
    hooks: &ManifestTransformHooks,
    request: &NewVersionRequest,
) -> Option<ResolvedVersion> {
    let name = request.name.to_string();
    let wanted = WantedDependency {
        alias: Some(name.clone()),
        bare_specifier: Some(request.specifier.clone()),
        ..WantedDependency::default()
    };
    let result = resolver.resolve(&wanted, resolve_options).await.ok()??;
    let manifest = Arc::clone(result.package.manifest.as_ref()?);
    let manifest = hooks
        .apply(manifest, || HookContext { log: Arc::new(|_| {}), dir: None })
        .await
        .ok()?;
    let version = result.package.name_ver.as_ref()?.suffix.to_string();
    if !is_safe_registry_result(&result, &manifest, &name, &version) {
        return None;
    }
    Some(ResolvedVersion {
        name: request.name.clone(),
        version,
        manifest,
        resolution: result.resolution,
    })
}

/// A copy of `lockfile` holding every resolved version, or `None` when one of
/// them cannot be recorded without a resolution.
fn add_resolved_versions(
    lockfile: &Lockfile,
    config: &Config,
    resolved: &[ResolvedVersion],
) -> Option<Lockfile> {
    let plan = RewritePlan {
        overrides: Vec::new(),
        peer_names: get_peer_names(lockfile),
        replacements: HashMap::new(),
    };
    let mut seeded = lockfile.clone();
    for version in resolved {
        let entry = locked_entry(lockfile, config, &plan, version)?;
        insert_entry(&mut seeded, entry)?;
    }
    Some(seeded)
}

/// The records for one resolved version. Its dependencies are matched against
/// the original lockfile, so a new version only ever depends on versions the
/// lockfile already held.
fn locked_entry(
    lockfile: &Lockfile,
    config: &Config,
    plan: &RewritePlan,
    version: &ResolvedVersion,
) -> Option<LockedEntry> {
    let snapshots = lockfile.snapshots.as_ref()?;
    let packages = lockfile.packages.as_ref()?;
    let key = PackageKey::new(version.name.clone(), version.version.parse().ok()?);
    let dependencies = validate_dependencies(
        effective_dependencies(&version.manifest)?,
        None,
        snapshots,
        packages,
        plan,
        &key,
    )?;
    let optional_dependencies = validate_dependencies(
        manifest_dependency_map(&version.manifest, "optionalDependencies")?,
        None,
        snapshots,
        packages,
        plan,
        &key,
    )?;
    let metadata = package_metadata(&version.manifest, lockfile_resolution(config, version)?);
    let snapshot =
        SnapshotEntry { dependencies, optional_dependencies, ..SnapshotEntry::default() };
    Some(LockedEntry { key, metadata, snapshot })
}

/// The resolution as the lockfile writes it for the registry the package is
/// routed to.
fn lockfile_resolution(config: &Config, version: &ResolvedVersion) -> Option<LockfileResolution> {
    let name = version.name.to_string();
    let registries: HashMap<String, String> = config
        .resolved_registries()
        .into_iter()
        .collect();
    let registry = pick_registry_for_package(&registries, &name, None);
    version.resolution
        .to_lockfile_form(
            &name,
            &version.version,
            LockfileFormOptions {
                registry: &registry,
                server_type: registry_server_type(&config.registry_options_by_url, &registry),
                include_tarball_url: config.lockfile_include_tarball_url,
            },
        )
        .ok()
}

/// Record the entry. `None` when the lockfile already holds that key, or that
/// package's metadata, in a different shape. Two ranges that resolve to the
/// same version record it once.
fn insert_entry(seeded: &mut Lockfile, entry: LockedEntry) -> Option<()> {
    let snapshots = seeded.snapshots.as_mut()?;
    let packages = seeded.packages.as_mut()?;
    let package_key = entry.key.without_peer();
    let conflicts = snapshots
        .get(&entry.key)
        .is_some_and(|existing| existing != &entry.snapshot)
        || packages
            .get(&package_key)
            .is_some_and(|existing| existing != &entry.metadata);
    if conflicts {
        return None;
    }
    packages.insert(package_key, entry.metadata);
    snapshots.insert(entry.key, entry.snapshot);
    Some(())
}

#[cfg(test)]
mod tests;
