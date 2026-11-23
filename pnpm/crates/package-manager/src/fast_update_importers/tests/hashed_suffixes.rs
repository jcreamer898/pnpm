use super::{manifest_from, parsed_lockfile, sorted_snapshot_keys, try_fast_update_importers};
use serde_json::json;

/// `baz` declares a peer on `react` and holds a suffix pnpm shortened into
/// a hash. Nothing declares `foo` as a peer.
const WITH_HASHED_PEER_SUFFIX: &str = r"
lockfileVersion: '9.0'
importers:
  .:
    dependencies:
      foo:
        specifier: ^1.0.0
        version: 1.1.0
      react:
        specifier: ^18.0.0
        version: 18.0.0
      baz:
        specifier: ^4.0.0
        version: 4.0.0(133ee57a755e64546d7eb65e940d1e32)
packages:
  foo@1.1.0:
    resolution:
      integrity: sha512-foo
  react@18.0.0:
    resolution:
      integrity: sha512-react
  baz@4.0.0:
    resolution:
      integrity: sha512-baz
    peerDependencies:
      react: '*'
snapshots:
  foo@1.1.0: {}
  react@18.0.0: {}
  baz@4.0.0(133ee57a755e64546d7eb65e940d1e32):
    dependencies:
      react: 18.0.0
";

/// The importer links `react` to `preact-compat`, so a suffix names the
/// package, while `baz` looks its peer up under the alias.
const WITH_PEER_SATISFIED_BY_AN_ALIAS: &str = r"
lockfileVersion: '9.0'
importers:
  .:
    dependencies:
      react:
        specifier: 'npm:preact-compat@^1.0.0'
        version: preact-compat@1.0.0
      baz:
        specifier: ^4.0.0
        version: 4.0.0(133ee57a755e64546d7eb65e940d1e32)
packages:
  preact-compat@1.0.0:
    resolution:
      integrity: sha512-preact-compat
  baz@4.0.0:
    resolution:
      integrity: sha512-baz
    peerDependencies:
      react: '*'
snapshots:
  preact-compat@1.0.0: {}
  baz@4.0.0(133ee57a755e64546d7eb65e940d1e32):
    dependencies:
      react: preact-compat@1.0.0
";

/// `foo` only reaches `preact-compat` through its own `react` link, which
/// `qux` makes too. The `baz` that `qux` holds is outside the subtree that
/// dropping `foo` severs.
const WITH_PEER_LINKED_INSIDE_A_SUBTREE: &str = r"
lockfileVersion: '9.0'
importers:
  .:
    dependencies:
      foo:
        specifier: ^1.0.0
        version: 1.1.0
      qux:
        specifier: ^2.0.0
        version: 2.0.0
packages:
  foo@1.1.0:
    resolution:
      integrity: sha512-foo
  qux@2.0.0:
    resolution:
      integrity: sha512-qux
  preact-compat@1.0.0:
    resolution:
      integrity: sha512-preact-compat
  baz@4.0.0:
    resolution:
      integrity: sha512-baz
    peerDependencies:
      react: '*'
snapshots:
  foo@1.1.0:
    dependencies:
      react: preact-compat@1.0.0
  qux@2.0.0:
    dependencies:
      baz: 4.0.0(133ee57a755e64546d7eb65e940d1e32)
      react: preact-compat@1.0.0
  preact-compat@1.0.0: {}
  baz@4.0.0(133ee57a755e64546d7eb65e940d1e32):
    dependencies:
      react: preact-compat@1.0.0
";

/// `foo` is a workspace sibling `baz` declares as a peer. A link has no
/// `name@version` to look for in a suffix, so only its alias is known.
const WITH_PEER_ON_A_LINK: &str = r"
lockfileVersion: '9.0'
importers:
  .:
    dependencies:
      foo:
        specifier: 'workspace:*'
        version: link:packages/foo
      baz:
        specifier: ^4.0.0
        version: 4.0.0(133ee57a755e64546d7eb65e940d1e32)
packages:
  baz@4.0.0:
    resolution:
      integrity: sha512-baz
    peerDependencies:
      foo: '*'
snapshots:
  baz@4.0.0(133ee57a755e64546d7eb65e940d1e32): {}
";

#[test]
fn drops_a_dependency_no_package_declares_as_a_peer_beside_a_hashed_suffix() {
    let manifest =
        manifest_from(json!({ "dependencies": { "baz": "^4.0.0", "react": "^18.0.0" } }));

    let updated = try_fast_update_importers(
        &parsed_lockfile(WITH_HASHED_PEER_SUFFIX),
        &[(".".to_string(), &manifest)],
    )
    .expect("a shortened suffix only embeds what satisfies a declared peer, and foo is none");

    assert_eq!(
        sorted_snapshot_keys(&updated),
        vec!["baz@4.0.0(133ee57a755e64546d7eb65e940d1e32)".to_string(), "react@18.0.0".to_string()],
    );
}

#[test]
fn rejects_dropping_a_dependency_a_hashed_suffix_may_embed_as_a_peer() {
    let manifest = manifest_from(json!({ "dependencies": { "foo": "^1.0.0", "baz": "^4.0.0" } }));

    assert!(
        try_fast_update_importers(
            &parsed_lockfile(WITH_HASHED_PEER_SUFFIX),
            &[(".".to_string(), &manifest)],
        )
        .is_none(),
        "baz declares react as a peer, so its shortened suffix may name the react that goes",
    );
}

#[test]
fn rejects_dropping_an_alias_a_hashed_suffix_may_embed_as_a_peer() {
    let manifest = manifest_from(json!({ "dependencies": { "baz": "^4.0.0" } }));

    assert!(
        try_fast_update_importers(
            &parsed_lockfile(WITH_PEER_SATISFIED_BY_AN_ALIAS),
            &[(".".to_string(), &manifest)],
        )
        .is_none(),
        "baz's peer is looked up as react, whatever package the importer links it to",
    );
}

#[test]
fn rejects_dropping_a_subtree_that_links_a_peer_a_hashed_suffix_may_embed() {
    let manifest = manifest_from(json!({ "dependencies": { "qux": "^2.0.0" } }));

    assert!(
        try_fast_update_importers(
            &parsed_lockfile(WITH_PEER_LINKED_INSIDE_A_SUBTREE),
            &[(".".to_string(), &manifest)],
        )
        .is_none(),
        "the dropped subtree reaches preact-compat as react, which baz's shortened suffix may name",
    );
}

#[test]
fn rejects_dropping_a_link_a_hashed_suffix_may_embed_as_a_peer() {
    let manifest = manifest_from(json!({ "dependencies": { "baz": "^4.0.0" } }));

    assert!(
        try_fast_update_importers(
            &parsed_lockfile(WITH_PEER_ON_A_LINK),
            &[(".".to_string(), &manifest)],
        )
        .is_none(),
        "baz declares foo as a peer, and nothing pins the link to a version",
    );
}
