//! Rotating weapon parts: Minigun `rotativePart.Rotate(0, 0, a)` (SM_Minigun_barrel) and
//! Propeller `rotativePart.Rotate(rotateSpeed, 0, 0)` (SM_Propeller00_Blades01). The angles are
//! accumulated by minigun.rs / propeller.rs; this applies them to the glb model nodes.
//! glb nodes are in AssetRipper's X-mirrored space: a Unity rotation about Z flips sign, about X doesn't.

use super::minigun::MinigunState;
use super::propeller::PropellerState;
use bevy::prelude::*;
use std::collections::HashMap;

#[derive(Default)]
pub struct SpinCache {
    /// weapon entity -> (node entity, base local rotation)
    nodes: HashMap<Entity, (Entity, Quat)>,
}

fn find_named(root: Entity, name: &str, children: &Query<&Children>, names: &Query<&Name>) -> Option<Entity> {
    let mut stack = vec![root];
    while let Some(e) = stack.pop() {
        if names.get(e).map(|n| n.as_str() == name).unwrap_or(false) {
            return Some(e);
        }
        if let Ok(k) = children.get(e) {
            stack.extend(k.iter());
        }
    }
    None
}

pub fn spin_parts(
    minis: Query<(Entity, &MinigunState)>,
    props: Query<(Entity, &PropellerState)>,
    children: Query<&Children>,
    names: Query<&Name>,
    mut tfs: Query<&mut Transform>,
    mut cache: Local<SpinCache>,
) {
    let mut apply = |weapon: Entity, node_name: &str, rot: Quat| {
        let entry = match cache.nodes.get(&weapon) {
            Some(v) => Some(*v),
            None => find_named(weapon, node_name, &children, &names).and_then(|n| tfs.get(n).ok().map(|t| (n, t.rotation))),
        };
        if let Some((node, base)) = entry {
            cache.nodes.insert(weapon, (node, base));
            if let Ok(mut t) = tfs.get_mut(node) {
                t.rotation = base * rot;
            }
        }
    };
    for (e, m) in &minis {
        apply(e, "SM_Minigun_barrel", Quat::from_rotation_z(-m.barrel_angle.to_radians()));
    }
    for (e, p) in &props {
        apply(e, "SM_Propeller00_Blades01", Quat::from_rotation_x(p.blade_angle.to_radians()));
    }
    cache.nodes.retain(|w, _| minis.contains(*w) || props.contains(*w));
}
