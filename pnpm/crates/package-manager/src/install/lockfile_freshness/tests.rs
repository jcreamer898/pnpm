use super::{WantedLockfileSatisfactionCheck, wanted_lockfile_satisfies_workspace};
use indexmap::IndexMap;
use pnpm_catalogs_types::Catalogs;
use pnpm_config::Config;
use pnpm_lockfile::Lockfile;
use pnpm_network::ThrottledClient;
use pnpm_package_manifest::PackageManifest;
use pnpm_reporter::SilentReporter;
use serde_json::json;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};
use tempfile::tempdir;

pub(crate) async fn check_frozen_fixture(
    check: &WantedLockfileSatisfactionCheck<'_>,
    manifests: &[(String, PackageManifest)],
) -> Result<(), String> {
    let manifests: Vec<_> = manifests
        .iter()
        .map(|(id, manifest)| (id.clone(), manifest))
        .collect();
    let declared: Vec<_> = manifests
        .iter()
        .map(|(_, manifest)| {
            (
                manifest
                    .path()
                    .parent()
                    .unwrap()
                    .to_path_buf(),
                *manifest,
            )
        })
        .collect();
    let unresolved = super::check_lockfile_freshness(
        check.lockfile,
        &super::LockfileFreshnessInputs {
            lockfile_dir: check.config.workspace_dir.as_deref().unwrap(),
            manifests: &manifests,
            workspace_packages: None,
            dependency_manifests: super::DependencyManifestSources {
                declared: &declared,
                injected: None,
            },
            config: check.config,
            catalogs: check.catalogs,
            pnpmfile_hook: None,
            scope: super::FreshnessScope {
                ignore_manifest_check: false,
                allow_missing_dependency_free_importers: false,
                frozen_lockfile: true,
                prune_stale_importers: true,
                patches_only: false,
                locked_registries: None,
            },
        },
    )
    .await
    .map_err(|error| format!("{error:?}"))?;
    assert!(unresolved.is_empty());
    Ok(())
}

#[tokio::test]
async fn workspace_satisfaction_uses_pinned_lockfile_dir_for_local_overrides() {
    let root = tempdir().expect("create fixture directory");
    let workspace_dir = root.path().join("workspace");
    let project_dir = workspace_dir.join("pkgs/a");
    let lockfile_dir = root.path().join("locks");
    fs::create_dir_all(&project_dir).expect("create nested project");
    fs::create_dir_all(&lockfile_dir).expect("create lockfile directory");
    fs::write(workspace_dir.join("package.json"), r#"{"name":"root","private":true}"#)
        .expect("write root manifest");
    fs::write(workspace_dir.join("pnpm-workspace.yaml"), "packages:\n  - pkgs/*\n")
        .expect("write workspace manifest");
    fs::write(project_dir.join("package.json"), r#"{"name":"a","dependencies":{"foo":"*"}}"#)
        .expect("write nested manifest");
    let manifest =
        PackageManifest::from_path(project_dir.join("package.json")).expect("read nested manifest");
    let mut config = Config::new();
    config.workspace_dir = Some(workspace_dir);
    config.lockfile_dir = Some(lockfile_dir);
    config.ignore_pnpmfile = true;
    config.overrides =
        Some(indexmap::IndexMap::from([("foo".to_string(), "link:./foo".to_string())]));
    let lockfile: Lockfile = serde_saphyr::from_str(
        r"lockfileVersion: '9.0'
overrides:
  foo: link:./foo
importers:
  ../workspace: {}
  ../workspace/pkgs/a:
    dependencies:
      foo:
        specifier: link:../../../locks/foo
        version: link:../../../locks/foo
",
    )
    .expect("parse lockfile");
    let catalogs = Catalogs::new();
    let check = WantedLockfileSatisfactionCheck {
        config: &config,
        manifest: &manifest,
        catalogs: &catalogs,
        lockfile: &lockfile,
        ignore_manifest_check: false,
    };
    assert!(
        wanted_lockfile_satisfies_workspace(&check).await,
        "a nested project's override must resolve from the pinned lockfile directory",
    );

    let mut stale_lockfile = lockfile.clone();
    stale_lockfile.importers
        .get_mut("../workspace/pkgs/a")
        .expect("nested importer")
        .dependencies
        .as_mut()
        .expect("dependencies")
        .values_mut()
        .next()
        .expect("foo dependency")
        .specifier = "link:foo".to_string();
    assert!(
        !wanted_lockfile_satisfies_workspace(&WantedLockfileSatisfactionCheck {
            lockfile: &stale_lockfile,
            ..check
        })
        .await,
        "an override incorrectly anchored at the nested project must be rejected",
    );
}

/// Regression test for [pnpm/pnpm#3960](https://github.com/pnpm/pnpm/issues/3960).
#[test]
fn unresolved_optional_dependency_is_satisfied_only_when_allowed() {
    let root = tempdir().expect("create fixture directory");
    fs::write(
        root.path().join("package.json"),
        r#"{"name":"root","dependencies":{"is-negative":"1.0.0"},"optionalDependencies":{"is-positive":"^30000.0.0"}}"#,
    )
    .expect("write manifest");
    let manifest =
        PackageManifest::from_path(root.path().join("package.json")).expect("read manifest");
    let lockfile: Lockfile = serde_saphyr::from_str(
        r"lockfileVersion: '9.0'
importers:
  .:
    dependencies:
      is-negative:
        specifier: 1.0.0
        version: 1.0.0
",
    )
    .expect("parse lockfile");
    let config = Config::new();
    let ignored_optional_matcher = pnpm_matcher::create_matcher(&[]);
    fn importer_check<'a>(
        (lockfile, lockfile_dir, config): (&'a Lockfile, &'a std::path::Path, &'a Config),
        manifest: &'a PackageManifest,
        (ignored, frozen_lockfile): (&'a pnpm_matcher::Matcher, bool),
    ) -> super::ImporterSatisfactionCheck<'a> {
        super::ImporterSatisfactionCheck {
            lockfile,
            lockfile_dir,
            manifest,
            importer_id: ".",
            config,
            workspace: super::WorkspaceProjects {
                peer_children: Box::leak(Box::default()),
                packages: None,
                manifests_by_dir: Box::leak(Box::default()),
                directory_manifests: crate::install::DirectoryManifests::none(),
                catalogs: Box::leak(Box::default()),
                canonical_paths: Box::leak(Box::default()),
                registries: Box::leak(Box::default()),
            },
            optional_exclusions: super::OptionalDependencyExclusions { ignored, frozen_lockfile },
            overrider: None,
        }
    }
    let fixture = (&lockfile, root.path(), &config);
    let check = |manifest: &PackageManifest, frozen_lockfile: bool| {
        super::check_importer_satisfies(&importer_check(
            fixture,
            manifest,
            (&ignored_optional_matcher, frozen_lockfile),
        ))
    };

    let error = check(&manifest, false).expect_err("a resolving install retries the optional");
    assert!(
        matches!(
            &error,
            super::FreshnessCheckError::Stale(pnpm_lockfile::StalenessReason::SpecifiersDiffer(
                diff
            )) if diff.added.contains_key("is-positive")
        ),
        "expected is-positive reported as added, got {error:?}",
    );
    let skipped = check(&manifest, true).expect("a frozen install skips the optional again");
    assert_eq!(skipped, vec![("is-positive".to_string(), "^30000.0.0".to_string())]);

    fs::write(
        root.path().join("package.json"),
        r#"{"name":"root","dependencies":{"is-negative":"1.0.0","is-odd":"1.0.0"},"optionalDependencies":{"is-positive":"^30000.0.0"}}"#,
    )
    .expect("write manifest with an added dependency");
    let manifest =
        PackageManifest::from_path(root.path().join("package.json")).expect("read manifest");
    let error = check(&manifest, true).expect_err("an added regular dependency is still drift");
    assert!(
        matches!(
            &error,
            super::FreshnessCheckError::Stale(pnpm_lockfile::StalenessReason::SpecifiersDiffer(
                diff
            )) if diff.added.contains_key("is-odd") && !diff.added.contains_key("is-positive")
        ),
        "expected only is-odd reported as added, got {error:?}",
    );
}

/// `foo` is recorded by its override and `bar` is the dependency the manifest
/// dropped.
const OVERRIDDEN_LOCKFILE: &str = r"
lockfileVersion: '9.0'
overrides:
  foo: 2.0.0
importers:
  .:
    dependencies:
      foo:
        specifier: 2.0.0
        version: 2.0.0
      bar:
        specifier: ^1.0.0
        version: 1.0.0
packages:
  foo@2.0.0:
    resolution:
      integrity: sha512-foo
  bar@1.0.0:
    resolution:
      integrity: sha512-bar
snapshots:
  foo@2.0.0: {}
  bar@1.0.0: {}
";

/// Run the fast update of `lockfile` for `manifest`, with `http_client` as the
/// client for registry lookups.
async fn fast_update(
    lockfile: &Lockfile,
    manifest: &PackageManifest,
    config: &Config,
    http_client: Option<Arc<ThrottledClient>>,
) -> Option<Lockfile> {
    let manifests = [(".".to_string(), manifest)];
    let project_manifests = [(PathBuf::from("/project"), manifest)];
    let catalogs = Catalogs::new();

    super::try_fast_update_lockfile::<SilentReporter>(super::FastUpdateLockfileOptions {
        lockfile: Some(lockfile),
        project_manifests: &project_manifests,
        freshness: super::LockfileFreshnessInputs {
            lockfile_dir: Path::new("/project"),
            manifests: &manifests,
            workspace_packages: None,
            dependency_manifests: super::DependencyManifestSources {
                declared: &project_manifests,
                injected: None,
            },
            config,
            catalogs: &catalogs,
            pnpmfile_hook: None,
            scope: super::FreshnessScope {
                ignore_manifest_check: false,
                allow_missing_dependency_free_importers: false,
                frozen_lockfile: false,
                prune_stale_importers: true,
                patches_only: false,
                locked_registries: None,
            },
        },
        http_client,
    })
    .await
}

fn sorted_snapshot_keys(lockfile: &Lockfile) -> Vec<String> {
    let mut keys: Vec<String> = lockfile.snapshots
        .as_ref()
        .expect("snapshots")
        .keys()
        .map(ToString::to_string)
        .collect();
    keys.sort();
    keys
}

#[tokio::test]
async fn fast_update_reads_the_manifests_with_the_overrides_applied() {
    let manifest = PackageManifest::from_value(
        PathBuf::from("/project/package.json"),
        json!({ "name": "root", "dependencies": { "foo": "^1.0.0" } }),
    );
    let mut config = Config::new();
    config.ignore_pnpmfile = true;
    config.overrides = Some(IndexMap::from([("foo".to_string(), "2.0.0".to_string())]));
    let lockfile: Lockfile = serde_saphyr::from_str(OVERRIDDEN_LOCKFILE).expect("parse lockfile");

    let updated = fast_update(&lockfile, &manifest, &config, None).await
        .expect("the removal is absorbed once `foo` reads as its override");

    assert_eq!(sorted_snapshot_keys(&updated), ["foo@2.0.0"]);
}

/// `foo` and the `bar` it depends on, locked once each.
const BUMPABLE_LOCKFILE: &str = r"
lockfileVersion: '9.0'
importers:
  .:
    dependencies:
      foo:
        specifier: ^1.0.0
        version: 1.1.0
      bar:
        specifier: ^1.0.0
        version: 1.0.0
packages:
  foo@1.1.0:
    resolution:
      integrity: sha512-foo
  bar@1.0.0:
    resolution:
      integrity: sha512-bar
snapshots:
  foo@1.1.0:
    dependencies:
      bar: 1.0.0
  bar@1.0.0: {}
";

/// Bump `foo` to `^1.2.0`, a range the lockfile holds no version of, against a
/// registry that serves `foo` 1.2.0 depending on `bar`.
async fn fast_update_bump_of_foo(http_client: Option<Arc<ThrottledClient>>) -> Option<Lockfile> {
    let mut server = mockito::Server::new_async().await;
    let registry = format!("{}/", server.url());
    let packument = json!({
        "name": "foo",
        "dist-tags": { "latest": "1.2.0" },
        "versions": {
            "1.2.0": {
                "name": "foo",
                "version": "1.2.0",
                "dependencies": { "bar": "^1.0.0" },
                "dist": {
                    "integrity": "sha512-dGFyZ2V0LTI=",
                    "tarball": format!("{registry}foo/-/foo-1.2.0.tgz"),
                },
            },
        },
    });
    let _packument = server
        .mock("GET", "/foo")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(packument.to_string())
        .create_async()
        .await;
    let cache = tempdir().expect("create cache dir");
    let mut config = Config::new();
    config.registry = registry;
    config.minimum_release_age = None;
    config.cache_dir = cache.path().join("cache");
    config.ignore_pnpmfile = true;
    let manifest = PackageManifest::from_value(
        PathBuf::from("/project/package.json"),
        json!({ "name": "root", "dependencies": { "foo": "^1.2.0", "bar": "^1.0.0" } }),
    );
    let lockfile: Lockfile = serde_saphyr::from_str(BUMPABLE_LOCKFILE).expect("parse lockfile");

    fast_update(&lockfile, &manifest, &config, http_client).await
}

#[tokio::test]
async fn fast_update_absorbs_a_bump_to_a_version_the_lockfile_never_held() {
    let updated = fast_update_bump_of_foo(Some(Arc::new(ThrottledClient::default())))
        .await
        .expect("the update looks the new version up through the client");

    assert_eq!(sorted_snapshot_keys(&updated), ["bar@1.0.0", "foo@1.2.0"]);
}

#[tokio::test]
async fn fast_update_without_a_client_leaves_a_bump_to_a_new_version_to_the_full_resolution() {
    assert!(fast_update_bump_of_foo(None).await.is_none());
}
