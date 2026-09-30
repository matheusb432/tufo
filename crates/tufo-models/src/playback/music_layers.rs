//! The two extra songs that can accompany the main playlist queue.

use super::player_state::MusicState;
use crate::library::Volume;

/// Includes the main playlist track.
pub const MUSIC_LAYERS_MAX: usize = 3;
pub const MUSIC_LAYERS_EXTRA_MAX: usize = MUSIC_LAYERS_MAX - 1;

#[derive(Debug, Clone, PartialEq)]
pub struct MusicLayerState {
    pub music: MusicState,
    pub volume: Volume,
    pub looping: bool,
}

/// Fixed slots keep a stopped layer from moving another layer to a different audio deck.
#[derive(Debug, Clone, PartialEq)]
pub struct MusicLayers<T> {
    slots: [Option<T>; MUSIC_LAYERS_EXTRA_MAX],
}

impl<T> Default for MusicLayers<T> {
    fn default() -> Self {
        Self {
            slots: std::array::from_fn(|_| None),
        }
    }
}

impl<T> MusicLayers<T> {
    /// Returns the occupied slot, or returns the value when every slot is occupied.
    pub fn try_insert(&mut self, value: T) -> Result<usize, T> {
        let Some(slot) = self.slots.iter().position(Option::is_none) else {
            return Err(value);
        };
        self.slots[slot] = Some(value);
        Ok(slot)
    }

    pub fn remove(&mut self, slot: usize) -> Option<T> {
        self.slots.get_mut(slot).and_then(Option::take)
    }

    pub fn get(&self, slot: usize) -> Option<&T> {
        self.slots.get(slot)?.as_ref()
    }

    pub fn get_mut(&mut self, slot: usize) -> Option<&mut T> {
        self.slots.get_mut(slot)?.as_mut()
    }

    pub fn iter(&self) -> impl Iterator<Item = (usize, &T)> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(slot, value)| value.as_ref().map(|value| (slot, value)))
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (usize, &mut T)> {
        self.slots
            .iter_mut()
            .enumerate()
            .filter_map(|(slot, value)| value.as_mut().map(|value| (slot, value)))
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.iter().count()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.iter().next().is_none()
    }

    pub fn map<U>(&self, mut map: impl FnMut(&T) -> U) -> MusicLayers<U> {
        MusicLayers {
            slots: std::array::from_fn(|slot| self.slots[slot].as_ref().map(&mut map)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_main_track_leaves_two_slots_and_removing_one_keeps_the_other_slot() {
        let mut layers = MusicLayers::default();
        assert_eq!(layers.try_insert("rain"), Ok(0));
        assert_eq!(layers.try_insert("wind"), Ok(1));
        assert_eq!(layers.try_insert("birds"), Err("birds"));
        assert_eq!(layers.remove(0), Some("rain"));
        assert_eq!(layers.iter().collect::<Vec<_>>(), vec![(1, &"wind")]);
        assert_eq!(layers.try_insert("birds"), Ok(0));
    }
}
