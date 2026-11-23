use super::{
    DroppedEdgeTarget, DroppedEdges, Lockfile, PkgName, PkgNameVerPeer, peer_id_of,
    reachable_from_keys, snapshot_child_keys,
};
use crate::fast_update_overrides::get_peer_names;
use std::collections::{HashMap, HashSet, VecDeque};

pub(crate) fn peer_sensitive_packages(lockfile: &Lockfile) -> HashSet<PkgNameVerPeer> {
    let mut parents: HashMap<PkgNameVerPeer, Vec<PkgNameVerPeer>> = HashMap::new();
    let mut queue = VecDeque::new();
    for (key, snapshot) in lockfile.snapshots.iter().flatten() {
        if has_declared_peers(lockfile, key) {
            queue.push_back(key.clone());
        }
        for child in snapshot_child_keys(snapshot, |_| true) {
            parents
                .entry(child)
                .or_default()
                .push(key.clone());
        }
    }
    let mut sensitive = HashSet::new();
    while let Some(key) = queue.pop_front() {
        if sensitive.insert(key.clone()) {
            queue.extend(
                parents
                    .remove(&key)
                    .into_iter()
                    .flatten(),
            );
        }
    }
    sensitive
        .into_iter()
        .map(|key| key.without_peer())
        .collect()
}

impl DroppedEdges {
    /// These edges, plus everything the severed packages reach, and every
    /// alias an edge anywhere in `lockfile` reaches one of them under.
    pub(super) fn including_subtrees(&self, lockfile: &Lockfile) -> Self {
        let mut affected = Self { targets: self.targets.clone(), names: self.names.clone() };
        let Some(snapshots) = &lockfile.snapshots else { return affected };
        let roots = snapshots
            .keys()
            .filter(|key| self.contains_package(key))
            .cloned()
            .collect();
        for key in reachable_from_keys(lockfile, roots, |_, _| true).into_iter().flatten() {
            affected.targets.insert(peer_id_of(&key).unwrap_or_else(|| format!("{}@", key.name)));
            affected.names.insert(key.name.to_string());
        }
        let aliases = aliases_reaching(lockfile, &affected);
        affected.names.extend(aliases);
        affected
    }

    /// Whether none of the names a severed package is known by is a name
    /// some package declares as a peer. A suffix pnpm shortened into a
    /// hash only embeds what satisfies a declared peer, found in the scope
    /// under the alias the peer is declared with, so it cannot embed a
    /// severed package then.
    pub(super) fn names_are_never_peers_in(&self, lockfile: &Lockfile) -> bool {
        let peer_names: HashSet<String> = get_peer_names(lockfile)
            .iter()
            .map(ToString::to_string)
            .collect();
        self.names
            .iter()
            .all(|name| !peer_names.contains(name))
    }
}

/// The alias of every importer and snapshot edge that reaches a package
/// `affected` names.
fn aliases_reaching(lockfile: &Lockfile, affected: &DroppedEdges) -> HashSet<String> {
    importer_edges(lockfile)
        .chain(snapshot_edges(lockfile))
        .filter(|(_, key)| {
            key.as_ref()
                .is_some_and(|key| affected.contains_package(key))
        })
        .map(|(alias, _)| alias.to_string())
        .collect()
}

/// Each edge as its alias and the snapshot key it points at, when it points at one.
type Edge<'a> = (&'a PkgName, Option<PkgNameVerPeer>);

fn importer_edges(lockfile: &Lockfile) -> impl Iterator<Item = Edge<'_>> {
    lockfile.importers
        .values()
        .flat_map(|importer| {
            [
                importer.dependencies.as_ref(),
                importer.dev_dependencies.as_ref(),
                importer.optional_dependencies.as_ref(),
            ]
            .into_iter()
            .flatten()
            .flatten()
        })
        .map(|(alias, spec)| (alias, spec.resolved_key(alias)))
}

fn snapshot_edges(lockfile: &Lockfile) -> impl Iterator<Item = Edge<'_>> {
    lockfile.snapshots
        .iter()
        .flat_map(|snapshots| snapshots.values())
        .flat_map(|snapshot| {
            [snapshot.dependencies.as_ref(), snapshot.optional_dependencies.as_ref()]
                .into_iter()
                .flatten()
                .flatten()
        })
        .map(|(alias, dep_ref)| (alias, dep_ref.resolved_key(alias)))
}

pub(super) fn has_declared_peers(lockfile: &Lockfile, key: &PkgNameVerPeer) -> bool {
    lockfile.packages
        .as_ref()
        .and_then(|packages| packages.get(&key.without_peer()))
        .and_then(|metadata| metadata.peer_dependencies.as_ref())
        .is_some_and(|peers| !peers.is_empty())
}
