use crate::{
    dcl::crdt::{
        entity::SceneEntityContainer, grow_only_set::GenericGrowOnlySetComponentOperation,
        last_write_wins::LastWriteWinsComponentOperation, SceneCrdtState,
        SceneCrdtStateProtoComponents,
    },
    scene_runner::scene::{Scene, SceneAvatarUpdates},
};

/// Applies the avatar entities that arrived at and left the scene to its entity container.
///
/// Arrivals are applied before departures on purpose:
/// - `try_init` is the only thing that marks a slot live, and `kill` only reports a death
///   (the DELETE_ENTITY the scene receives, which is also what drops the entity's
///   components from the state) for a slot that is live.
/// - `try_init` of a newer version buries the previous occupant of a recycled slot by
///   itself, so going in this order keeps the churn correct both when an avatar joins and
///   leaves within one tick and when a slot is handed over to another peer within one tick.
pub fn update_avatar_scene_entities(
    entities: &mut SceneEntityContainer,
    avatar_scene_updates: &mut SceneAvatarUpdates,
) {
    for entity_id in avatar_scene_updates.created_entities.drain() {
        entities.try_init(entity_id);
    }

    for entity_id in avatar_scene_updates.deleted_entities.drain() {
        entities.kill(entity_id);
    }
}

pub fn update_avatar_scene_updates(scene: &mut Scene, crdt_state: &mut SceneCrdtState) {
    update_avatar_scene_entities(&mut crdt_state.entities, &mut scene.avatar_scene_updates);

    {
        let transform_component = crdt_state.get_transform_mut();
        for (entity_id, value) in scene.avatar_scene_updates.transform.drain() {
            transform_component.put(entity_id, value);
        }
    }

    {
        let avatar_base_component = SceneCrdtStateProtoComponents::get_avatar_base_mut(crdt_state);
        for (entity_id, value) in scene.avatar_scene_updates.avatar_base.drain() {
            avatar_base_component.put(entity_id, Some(value));
        }
    }

    {
        let player_identity_data_component =
            SceneCrdtStateProtoComponents::get_player_identity_data_mut(crdt_state);
        for (entity_id, value) in scene.avatar_scene_updates.player_identity_data.drain() {
            player_identity_data_component.put(entity_id, Some(value));
        }
    }

    {
        let avatar_equipped_data_component =
            SceneCrdtStateProtoComponents::get_avatar_equipped_data_mut(crdt_state);
        for (entity_id, value) in scene.avatar_scene_updates.avatar_equipped_data.drain() {
            avatar_equipped_data_component.put(entity_id, Some(value));
        }
    }

    {
        let internal_player_data_component = crdt_state.get_internal_player_data_mut();
        for (entity_id, value) in scene.avatar_scene_updates.internal_player_data.drain() {
            internal_player_data_component.put(entity_id, Some(value));
        }
    }

    {
        let avatar_emote_command_component =
            SceneCrdtStateProtoComponents::get_avatar_emote_command_mut(crdt_state);
        for (entity_id, vec_value) in scene.avatar_scene_updates.avatar_emote_command.drain() {
            let mut timestamp: u32 = {
                if let Some(commands) = avatar_emote_command_component.get(&entity_id) {
                    commands.iter().map(|c| c.timestamp).max().unwrap_or(0) + 1
                } else {
                    0
                }
            };

            for mut value in vec_value {
                value.timestamp = timestamp;
                timestamp += 1;
                avatar_emote_command_component.append(entity_id, value);
            }
        }
    }
}

#[cfg(test)]
mod test {
    use std::collections::HashSet;

    use super::*;
    use crate::dcl::components::SceneEntityId;

    // Avatars get entity numbers in [FROM_ENTITY_ID, MAX_ENTITY_ID), see
    // `AvatarScene::get_next_entity_id`.
    const AVATAR: u16 = 32;

    // Applies one scene tick worth of avatar entity churn and returns the (born, died) sets
    // the scene thread is handed. Every `died` entry becomes a DELETE_ENTITY for the scene.
    fn tick(
        entities: &mut SceneEntityContainer,
        created: &[SceneEntityId],
        deleted: &[SceneEntityId],
    ) -> (HashSet<SceneEntityId>, HashSet<SceneEntityId>) {
        let mut updates = SceneAvatarUpdates {
            created_entities: created.iter().copied().collect(),
            deleted_entities: deleted.iter().copied().collect(),
            ..Default::default()
        };

        update_avatar_scene_entities(entities, &mut updates);

        let dirty = entities.take_dirty();
        (dirty.born, dirty.died)
    }

    #[test]
    fn test_avatar_joining_after_the_first_tick_is_announced_when_it_leaves() {
        // A scene that already ran its first tick while nobody else was connected: its
        // `first_sync_crdt_state` found no live avatar and left every slot untouched.
        let mut entities = SceneEntityContainer::new();
        let avatar = SceneEntityId::new(AVATAR, 0);

        // The peer joins and `add_avatar` fans the creation out to the scene.
        tick(&mut entities, &[avatar], &[]);

        // The peer leaves and `remove_avatar` fans the deletion out to the scene, which has
        // to be told about it.
        assert_eq!(
            tick(&mut entities, &[], &[avatar]),
            (HashSet::new(), HashSet::from([avatar]))
        );
        assert_eq!(entities.get_entity_stat(AVATAR), &(1, false));
    }

    #[test]
    fn test_avatar_creation_marks_the_slot_live_in_the_scene() {
        let mut entities = SceneEntityContainer::new();
        let avatar = SceneEntityId::new(AVATAR, 0);

        assert_eq!(
            tick(&mut entities, &[avatar], &[]),
            (HashSet::from([avatar]), HashSet::new())
        );
        assert_eq!(entities.get_entity_stat(AVATAR), &(0, true));
    }

    #[test]
    fn test_second_peer_recycling_the_slot_is_announced_when_it_leaves() {
        let mut entities = SceneEntityContainer::new();
        let first = SceneEntityId::new(AVATAR, 0);
        // `get_next_entity_id` hands the freed slot over with the version the previous kill
        // bumped it to.
        let second = SceneEntityId::new(AVATAR, 1);

        tick(&mut entities, &[first], &[]);
        tick(&mut entities, &[], &[first]);

        tick(&mut entities, &[second], &[]);
        assert_eq!(
            tick(&mut entities, &[], &[second]),
            (HashSet::new(), HashSet::from([second]))
        );
    }

    #[test]
    fn test_peer_joining_and_leaving_within_the_same_tick_is_announced_once() {
        let mut entities = SceneEntityContainer::new();
        let avatar = SceneEntityId::new(AVATAR, 0);

        // Creations are applied before deletions, so the slot is live by the time it is
        // killed and the scene gets exactly one death instead of nothing.
        assert_eq!(
            tick(&mut entities, &[avatar], &[avatar]),
            (HashSet::new(), HashSet::from([avatar]))
        );
    }

    #[test]
    fn test_slot_handed_over_within_the_same_tick_buries_the_previous_peer() {
        let mut entities = SceneEntityContainer::new();
        let first = SceneEntityId::new(AVATAR, 0);
        let second = SceneEntityId::new(AVATAR, 1);

        tick(&mut entities, &[first], &[]);

        // `try_init` of the newer version buries the previous occupant by itself, so the
        // stale kill of the older version is a no-op and no death is lost.
        assert_eq!(
            tick(&mut entities, &[second], &[first]),
            (HashSet::from([second]), HashSet::from([first]))
        );
    }

    #[test]
    fn test_applied_updates_are_drained() {
        let mut entities = SceneEntityContainer::new();
        let mut updates = SceneAvatarUpdates {
            created_entities: HashSet::from([SceneEntityId::new(AVATAR, 0)]),
            deleted_entities: HashSet::from([SceneEntityId::new(AVATAR + 1, 0)]),
            ..Default::default()
        };

        update_avatar_scene_entities(&mut entities, &mut updates);

        assert!(updates.created_entities.is_empty());
        assert!(updates.deleted_entities.is_empty());
    }
}
