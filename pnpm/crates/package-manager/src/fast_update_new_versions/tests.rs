use super::{
    NewVersionOptions, NewVersionRequest, ResolvedVersion, add_resolved_versions,
    new_version_requests, requested_versions, seed_new_versions,
};
use pnpm_config::{Config, TrustPolicy};
use pnpm_lockfile::{Lockfile, LockfileResolution, PkgName, TarballResolution};
use pnpm_network::ThrottledClient;
use pnpm_package_manifest::PackageManifest;
use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};
use tempfile::tempdir;

/// `foo`, `bar` and `baz` locked once each, with `foo` depending on `bar`.
const LOCKFILE: &str = r"
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
      baz:
        specifier: ^1.0.0
        version: 1.0.0
packages:
  foo@1.1.0:
    resolution:
      integrity: sha512-foo
  bar@1.0.0:
    resolution:
      integrity: sha512-bar
  baz@1.0.0:
    resolution:
      integrity: sha512-baz
snapshots:
  foo@1.1.0:
    dependencies:
      bar: 1.0.0
  bar@1.0.0: {}
  baz@1.0.0: {}
";

/// [`LOCKFILE`] once `foo` 1.2.0 is added, depending on `bar` and optionally
/// on `baz`. `foo` 1.1.0 stays, since another importer may still use it.
const WITH_FOO_1_2_0: &str = r"
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
      baz:
        specifier: ^1.0.0
        version: 1.0.0
packages:
  foo@1.1.0:
    resolution:
      integrity: sha512-foo
  foo@1.2.0:
    resolution:
      integrity: sha512-dGFyZ2V0LTI=
  bar@1.0.0:
    resolution:
      integrity: sha512-bar
  baz@1.0.0:
    resolution:
      integrity: sha512-baz
snapshots:
  foo@1.1.0:
    dependencies:
      bar: 1.0.0
  foo@1.2.0:
    dependencies:
      bar: 1.0.0
    optionalDependencies:
      baz: 1.0.0
  bar@1.0.0: {}
  baz@1.0.0: {}
";

/// The importer records `foo` under `^1.2.0` at 1.1.0, a version the range
/// does not accept, as an override can leave it.
const WITH_FOO_BELOW_ITS_RANGE: &str = r"
lockfileVersion: '9.0'
importers:
  .:
    dependencies:
      foo:
        specifier: ^1.2.0
        version: 1.1.0
packages:
  foo@1.1.0:
    resolution:
      integrity: sha512-foo
snapshots:
  foo@1.1.0: {}
";

/// Two importers resolve `bar` to different versions that `^1.0.0` accepts.
const WITH_TWO_BARS: &str = r"
lockfileVersion: '9.0'
importers:
  .:
    dependencies:
      bar:
        specifier: ^1.0.0
        version: 1.1.0
  packages/a:
    dependencies:
      bar:
        specifier: 1.0.0
        version: 1.0.0
packages:
  bar@1.0.0:
    resolution:
      integrity: sha512-bar
  bar@1.1.0:
    resolution:
      integrity: sha512-bar
snapshots:
  bar@1.0.0: {}
  bar@1.1.0: {}
";

fn manifest(value: Value) -> PackageManifest {
    PackageManifest::from_value(PathBuf::from("/project/package.json"), value)
}

fn parsed_lockfile(source: &str) -> Lockfile {
    serde_saphyr::from_str(source).expect("parse lockfile")
}

/// A project that bumps `foo` to a range the lockfile does not satisfy.
fn bump() -> PackageManifest {
    manifest(json!({ "dependencies": { "foo": "^1.2.0" } }))
}

fn described(requests: Vec<NewVersionRequest>) -> Vec<String> {
    let mut described: Vec<String> = requests
        .into_iter()
        .map(|request| format!("{}@{}", request.name, request.specifier))
        .collect();
    described.sort();
    described
}

/// The requests a single root project makes against `lockfile`.
fn requests_in(lockfile: &Lockfile, value: Value) -> Vec<String> {
    let manifest = manifest(value);
    described(new_version_requests(lockfile, &[(".".to_string(), &manifest)]))
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

/// The manifest the registry serves for `foo` 1.2.0.
fn foo_manifest() -> Value {
    json!({
        "name": "foo",
        "version": "1.2.0",
        "dependencies": { "bar": "^1.0.0" },
        "optionalDependencies": { "baz": "^1.0.0" },
    })
}

fn resolved_version(name: &str, version: &str, manifest: Value) -> ResolvedVersion {
    ResolvedVersion {
        name: name.parse().expect("package name"),
        version: version.to_string(),
        manifest: Arc::new(manifest),
        resolution: LockfileResolution::Tarball(TarballResolution {
            tarball: format!("https://registry.npmjs.org/{name}/-/{name}-{version}.tgz"),
            integrity: Some("sha512-dGFyZ2V0LTI=".parse().expect("integrity")),
            revision: None,
            git_hosted: None,
            path: None,
        }),
    }
}

fn foo_1_2_0() -> ResolvedVersion {
    resolved_version("foo", "1.2.0", foo_manifest())
}

#[test]
fn requests_a_range_no_locked_version_satisfies_from_either_dependency_group() {
    let requests = requests_in(
        &parsed_lockfile(LOCKFILE),
        json!({
            "dependencies": { "foo": "^1.2.0", "bar": "^1.0.0" },
            "devDependencies": { "qux": "^2.0.0" },
        }),
    );

    assert_eq!(requests, ["foo@^1.2.0", "qux@^2.0.0"]);
}

#[test]
fn skips_a_range_a_locked_version_satisfies() {
    let requests = requests_in(
        &parsed_lockfile(LOCKFILE),
        json!({ "dependencies": { "foo": ">=1.0.0 <2.0.0" } }),
    );

    assert_eq!(requests, Vec::<String>::new());
}

#[test]
fn skips_a_declaration_the_importer_already_records() {
    let lockfile = parsed_lockfile(WITH_FOO_BELOW_ITS_RANGE);

    assert_eq!(
        requests_in(&lockfile, json!({ "dependencies": { "foo": "^1.2.0" } })),
        Vec::<String>::new(),
    );
    assert_eq!(
        requests_in(&lockfile, json!({ "dependencies": { "foo": "^1.3.0" } })),
        ["foo@^1.3.0"],
    );
}

#[test]
fn skips_specifiers_that_are_not_plain_version_ranges() {
    let requests = requests_in(
        &parsed_lockfile(LOCKFILE),
        json!({
            "dependencies": {
                "tagged": "latest",
                "sibling": "workspace:*",
                "aliased": "npm:other@^1.0.0",
                "hosted": "github:user/repo",
                "copied": "file:../x",
                "linked": "link:../x",
            },
            "peerDependencies": { "peer": "^1.0.0" },
        }),
    );

    assert_eq!(requests, Vec::<String>::new());
}

#[test]
fn requests_a_range_two_projects_share_once() {
    let lockfile = parsed_lockfile(LOCKFILE);
    let root = manifest(json!({ "dependencies": { "qux": "^2.0.0" } }));
    let nested = manifest(json!({ "dependencies": { "qux": "^2.0.0" } }));

    let requests = new_version_requests(
        &lockfile,
        &[(".".to_string(), &root), ("packages/a".to_string(), &nested)],
    );

    assert_eq!(described(requests), ["qux@^2.0.0"]);
}

#[test]
fn leaves_optional_dependencies_to_the_full_resolution() {
    let requests = requests_in(
        &parsed_lockfile(LOCKFILE),
        json!({
            "dependencies": { "foo": "^1.2.0", "both": "^3.0.0" },
            "optionalDependencies": { "extra": "^2.0.0", "both": "^3.0.0" },
        }),
    );

    assert_eq!(requests, ["foo@^1.2.0"]);
}

#[test]
fn requests_nothing_for_a_lockfile_that_records_publish_times() {
    let mut lockfile = parsed_lockfile(LOCKFILE);
    let bump = bump();
    let manifests = [(".".to_string(), &bump)];
    assert!(requested_versions(&lockfile, &manifests, &Config::new()).is_some());

    lockfile.time =
        Some(BTreeMap::from([("foo@1.1.0".to_string(), "2025-01-01T00:00:00.000Z".to_string())]));

    assert!(requested_versions(&lockfile, &manifests, &Config::new()).is_none());
}

#[test]
fn requests_nothing_under_a_trust_policy() {
    let lockfile = parsed_lockfile(LOCKFILE);
    let bump = bump();
    let manifests = [(".".to_string(), &bump)];
    let mut config = Config::new();
    assert!(requested_versions(&lockfile, &manifests, &config).is_some());

    config.trust_policy = TrustPolicy::NoDowngrade;

    assert!(requested_versions(&lockfile, &manifests, &config).is_none());
}

#[test]
fn requests_nothing_when_a_workspace_project_has_the_requested_name() {
    let lockfile = parsed_lockfile(LOCKFILE);
    let bump = bump();
    let sibling = manifest(json!({ "name": "foo", "version": "1.2.0" }));
    assert!(requested_versions(&lockfile, &[(".".to_string(), &bump)], &Config::new()).is_some());

    let with_sibling = [(".".to_string(), &bump), ("packages/foo".to_string(), &sibling)];

    assert!(requested_versions(&lockfile, &with_sibling, &Config::new()).is_none());
}

#[test]
fn requests_nothing_when_every_declared_range_is_already_locked() {
    let locked = manifest(json!({ "dependencies": { "bar": "^1.0.0" } }));

    let requests = requested_versions(
        &parsed_lockfile(LOCKFILE),
        &[(".".to_string(), &locked)],
        &Config::new(),
    );

    assert!(requests.is_none());
}

#[test]
fn adds_a_resolved_version_beside_the_locked_ones() {
    let seeded = add_resolved_versions(&parsed_lockfile(LOCKFILE), &Config::new(), &[foo_1_2_0()])
        .expect("both dependencies of foo 1.2.0 are locked once");

    assert_eq!(seeded, parsed_lockfile(WITH_FOO_1_2_0));
}

#[test]
fn lets_the_compose_retarget_an_importer_at_a_seeded_version() {
    let seeded = add_resolved_versions(&parsed_lockfile(LOCKFILE), &Config::new(), &[foo_1_2_0()])
        .expect("both dependencies of foo 1.2.0 are locked once");
    let bumped = manifest(json!({
        "dependencies": { "foo": "^1.2.0", "bar": "^1.0.0", "baz": "^1.0.0" },
    }));

    let composed = crate::fast_update_compose::try_compose_fast_updates(
        &seeded,
        &[(".".to_string(), &bumped)],
        &[],
        &Config::new(),
        None,
        false,
    )
    .expect("the bump is absorbed once the lockfile holds foo 1.2.0");

    let foo: PkgName = "foo".parse().expect("package name");
    let recorded = &composed.importers["."].dependencies.as_ref().expect("dependencies")[&foo];
    assert_eq!(recorded.specifier, "^1.2.0");
    assert_eq!(recorded.version.to_string(), "1.2.0");
    assert_eq!(sorted_snapshot_keys(&composed), ["bar@1.0.0", "baz@1.0.0", "foo@1.2.0"]);
}

#[test]
fn records_a_version_resolved_twice_once() {
    let resolved = [foo_1_2_0(), foo_1_2_0()];

    let seeded = add_resolved_versions(&parsed_lockfile(LOCKFILE), &Config::new(), &resolved)
        .expect("two identical records of one version do not conflict");

    assert_eq!(seeded, parsed_lockfile(WITH_FOO_1_2_0));
}

#[test]
fn declines_two_different_records_of_one_version() {
    let other = resolved_version(
        "foo",
        "1.2.0",
        json!({ "name": "foo", "version": "1.2.0", "dependencies": { "baz": "^1.0.0" } }),
    );

    let seeded =
        add_resolved_versions(&parsed_lockfile(LOCKFILE), &Config::new(), &[foo_1_2_0(), other]);

    assert!(seeded.is_none());
}

#[test]
fn declines_a_version_with_a_dependency_the_lockfile_does_not_hold() {
    let needs_qux = resolved_version(
        "foo",
        "1.2.0",
        json!({ "name": "foo", "version": "1.2.0", "dependencies": { "qux": "^1.0.0" } }),
    );

    let seeded = add_resolved_versions(&parsed_lockfile(LOCKFILE), &Config::new(), &[needs_qux]);

    assert!(seeded.is_none());
}

#[test]
fn declines_a_version_whose_dependency_has_two_locked_candidates() {
    let seeded =
        add_resolved_versions(&parsed_lockfile(WITH_TWO_BARS), &Config::new(), &[foo_1_2_0()]);

    assert!(seeded.is_none(), "a full resolution may pick either bar, so the choice is not local");
}

/// The packument the registry serves for `foo`, whose only version has
/// `version_manifest` as its manifest.
fn packument(registry: &str, mut version_manifest: Value) -> String {
    version_manifest["dist"] = json!({
        "integrity": "sha512-dGFyZ2V0LTI=",
        "tarball": format!("{registry}foo/-/foo-1.2.0.tgz"),
    });
    json!({
        "name": "foo",
        "dist-tags": { "latest": "1.2.0" },
        "versions": { "1.2.0": version_manifest },
    })
    .to_string()
}

/// Seed the bump from `server`, which stands in for the registry.
async fn seed_from(server: &mockito::Server) -> Option<Lockfile> {
    let cache = tempdir().expect("create cache dir");
    let mut config = Config::new();
    config.registry = format!("{}/", server.url());
    config.minimum_release_age = None;
    config.fetch_retry_mintimeout = 1;
    config.fetch_retry_maxtimeout = 1;
    config.cache_dir = cache.path().join("cache");
    let lockfile = parsed_lockfile(LOCKFILE);
    let bump = bump();
    let manifests = [(".".to_string(), &bump)];
    let hooks = crate::install_with_fresh_lockfile::resolver_manifest_hooks(&config, None)
        .expect("manifest hooks")
        .with_pnpmfile(None);

    seed_new_versions(NewVersionOptions {
        lockfile: &lockfile,
        manifests: &manifests,
        config: &config,
        http_client: Arc::new(ThrottledClient::default()),
        hooks,
        lockfile_dir: Path::new("/project"),
    })
    .await
}

/// Seed the bump from a registry that serves `foo` with `version_manifest`.
async fn seed_from_registry(version_manifest: Value) -> Option<Lockfile> {
    let mut server = mockito::Server::new_async().await;
    let registry = format!("{}/", server.url());
    let _packument = server
        .mock("GET", "/foo")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(packument(&registry, version_manifest))
        .create_async()
        .await;

    seed_from(&server).await
}

#[tokio::test]
async fn seeds_the_version_the_registry_picks_for_an_unlocked_range() {
    let seeded = seed_from_registry(foo_manifest()).await
        .expect("the registry serves a version whose dependencies are locked");

    assert_eq!(seeded, parsed_lockfile(WITH_FOO_1_2_0));
}

#[tokio::test]
async fn declines_a_version_the_registry_serves_with_peer_dependencies() {
    let mut with_peers = foo_manifest();
    with_peers["peerDependencies"] = json!({ "react": "^18.0.0" });

    assert!(seed_from_registry(with_peers).await.is_none());
}

#[tokio::test]
async fn declines_after_one_attempt_when_the_registry_fails() {
    let mut server = mockito::Server::new_async().await;
    let lookups = server
        .mock("GET", "/foo")
        .with_status(503)
        .expect(1)
        .create_async()
        .await;

    let seeded = seed_from(&server).await;

    assert!(seeded.is_none());
    lookups.assert_async().await;
}
