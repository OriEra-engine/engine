//! World snapshots (issue #12) — the L3 `Serialize` contract from book
//! T6 / appendix B, in the narrow v0.1 shape: `Position` + `Velocity`,
//! plus the entity bookkeeping needed to keep handles honest.
//!
//! # Why a hand-written format
//!
//! The issue allowed `postcard` or a hand-written binary. Hand-written
//! wins on one property that matters more than ergonomics here:
//! **byte-for-byte determinism**. `snapshot()` on two identical worlds
//! must emit identical bytes, because the snapshot's job is to be
//! comparable (replay, multi-peer checks, golden anchors) — a format
//! whose output drifts with a dependency bump would silently break
//! those comparisons. The wire shape is versioned (`SNAPSHOT_VERSION`)
//! and magic-tagged, so adding a column later is a version bump, not a
//! guess.
//!
//! # What is *not* in the snapshot
//!
//! Resources and event queues: they hold `Any` values with no
//! serialization contract yet, and #12 scope is the standard component
//! columns. Restoring therefore produces a world with empty resources
//! and empty event queues — document it here so nobody assumes a
//! restored world keeps its `Gravity` resource.
//!
//! Generations and the free list *are* included even though
//! [`World::state_hash`] ignores them: `state_hash` answers "same
//! simulation state", while a snapshot must also answer "same handle
//! semantics" — a restored world that hands out stale-accepting handles
//! would be a far worse bug than a hash mismatch.

use std::collections::HashMap;
use std::fmt;

use crate::{Column, Position, Velocity, World};

/// Magic head of every snapshot: `ORIECS01` (OriEra ECS, format v1).
const SNAPSHOT_MAGIC: [u8; 8] = *b"ORIECS01";

/// Bumped whenever the wire layout changes. A reader refuses anything
/// it was not written to understand.
const SNAPSHOT_VERSION: u32 = 1;

/// Why a snapshot could not be read.
///
/// Every variant means "the bytes are not a v1 snapshot this code
/// understands" — there is no partial restore: a failed `restore`
/// leaves the world exactly as it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotError {
    /// Header does not spell `ORIECS01`.
    BadMagic,
    /// Right magic, unknown layout version.
    UnsupportedVersion(u32),
    /// Ran out of bytes mid-field.
    Truncated,
    /// Bytes remain after the last documented field.
    TrailingBytes,
    /// Structurally readable but violates an invariant (free index past
    /// `next_index`, `live_count` above entity count, oversized column).
    Corrupt(&'static str),
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadMagic => write!(f, "not an OriEra world snapshot (bad magic)"),
            Self::UnsupportedVersion(v) => write!(f, "unsupported snapshot version {v}"),
            Self::Truncated => write!(f, "snapshot ends mid-field"),
            Self::TrailingBytes => write!(f, "snapshot has trailing bytes"),
            Self::Corrupt(why) => write!(f, "snapshot violates an invariant: {why}"),
        }
    }
}

impl std::error::Error for SnapshotError {}

impl World {
    /// Serialize this world's simulation state into a v1 snapshot.
    ///
    /// Deterministic: identical worlds emit identical bytes. Allocation
    /// is one `Vec`, sized up front from the entity count.
    pub fn snapshot(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(40 + self.generations.len() * 12 + self.free.len() * 4);
        out.extend_from_slice(&SNAPSHOT_MAGIC);
        out.extend_from_slice(&SNAPSHOT_VERSION.to_le_bytes());
        out.extend_from_slice(&self.next_index.to_le_bytes());
        out.extend_from_slice(&(self.generations.len() as u32).to_le_bytes());
        for generation in &self.generations {
            out.extend_from_slice(&generation.to_le_bytes());
        }
        out.extend_from_slice(&(self.free.len() as u32).to_le_bytes());
        for index in &self.free {
            out.extend_from_slice(&index.to_le_bytes());
        }
        out.extend_from_slice(&(self.live_count as u64).to_le_bytes());
        write_column::<Position>(&self.columns, &mut out, |p| (p.x, p.y));
        write_column::<Velocity>(&self.columns, &mut out, |v| (v.vx, v.vy));
        out
    }

    /// Rebuild a world from [`World::snapshot`] bytes.
    pub fn from_snapshot(data: &[u8]) -> Result<Self, SnapshotError> {
        let mut reader = Reader::new(data);
        let magic = reader.take(8)?;
        if magic != SNAPSHOT_MAGIC {
            return Err(SnapshotError::BadMagic);
        }
        let version = reader.u32()?;
        if version != SNAPSHOT_VERSION {
            return Err(SnapshotError::UnsupportedVersion(version));
        }

        let next_index = reader.u32()?;
        let generations_len = reader.u32()? as usize;
        let mut generations = Vec::with_capacity(generations_len.min(reader.remaining() / 4));
        for _ in 0..generations_len {
            generations.push(reader.u32()?);
        }

        let free_len = reader.u32()? as usize;
        let mut free = Vec::with_capacity(free_len.min(reader.remaining() / 4));
        for _ in 0..free_len {
            let index = reader.u32()?;
            if index >= next_index {
                return Err(SnapshotError::Corrupt("free index past next_index"));
            }
            free.push(index);
        }

        let live_count = reader.u64()?;
        if live_count > u64::from(next_index) {
            return Err(SnapshotError::Corrupt("live_count above entity count"));
        }

        let mut columns: HashMap<std::any::TypeId, Box<dyn Column>> = HashMap::new();
        let positions =
            read_column::<Position>(&mut reader, generations_len, |a, b| Position { x: a, y: b })?;
        let velocities = read_column::<Velocity>(&mut reader, generations_len, |a, b| Velocity {
            vx: a,
            vy: b,
        })?;
        if !positions.is_empty() {
            columns.insert(
                std::any::TypeId::of::<Position>(),
                Box::new(positions) as Box<dyn Column>,
            );
        }
        if !velocities.is_empty() {
            columns.insert(
                std::any::TypeId::of::<Velocity>(),
                Box::new(velocities) as Box<dyn Column>,
            );
        }

        if reader.remaining() != 0 {
            return Err(SnapshotError::TrailingBytes);
        }

        Ok(Self {
            next_index,
            generations,
            free,
            live_count: live_count as usize,
            columns,
            resources: HashMap::new(),
            events: HashMap::new(),
        })
    }

    /// Replace this world with the snapshot's contents.
    ///
    /// All-or-nothing: on `Err` the world is untouched (the candidate is
    /// fully built before the assignment), so a corrupt save can never
    /// leave a half-restored world behind. Resources and events are not
    /// part of the format and are dropped on success.
    pub fn restore(&mut self, data: &[u8]) -> Result<(), SnapshotError> {
        let restored = Self::from_snapshot(data)?;
        *self = restored;
        Ok(())
    }
}

/// Write one standard column: `u32 len`, then `len × (u8 present, [x, y])`.
///
/// Floats travel as raw `to_bits` words, so `-0.0`, subnormals and NaNs
/// survive round-trip and keep the state hash bit-equal.
fn write_column<T: 'static>(
    columns: &HashMap<std::any::TypeId, Box<dyn Column>>,
    out: &mut Vec<u8>,
    read: impl Fn(&T) -> (f32, f32),
) {
    let column = columns
        .get(&std::any::TypeId::of::<T>())
        .and_then(|c| c.as_any().downcast_ref::<Vec<Option<T>>>());
    let Some(column) = column else {
        out.extend_from_slice(&0u32.to_le_bytes());
        return;
    };
    out.extend_from_slice(&(column.len() as u32).to_le_bytes());
    for slot in column {
        match slot {
            None => out.push(0),
            Some(value) => {
                out.push(1);
                let (a, b) = read(value);
                out.extend_from_slice(&a.to_bits().to_le_bytes());
                out.extend_from_slice(&b.to_bits().to_le_bytes());
            }
        }
    }
}

/// Read one standard column back, sized to `expected_len` entity slots.
fn read_column<T: 'static>(
    reader: &mut Reader<'_>,
    expected_len: usize,
    build: impl Fn(f32, f32) -> T,
) -> Result<Vec<Option<T>>, SnapshotError> {
    let len = reader.u32()? as usize;
    if len > expected_len {
        return Err(SnapshotError::Corrupt(
            "column longer than the entity table",
        ));
    }
    let mut column: Vec<Option<T>> = Vec::with_capacity(len);
    for _ in 0..len {
        column.push(match reader.u8()? {
            0 => None,
            1 => Some(build(reader.f32()?, reader.f32()?)),
            _ => return Err(SnapshotError::Corrupt("presence flag is not 0/1")),
        });
    }
    Ok(column)
}

/// Bounds-checked little-endian cursor. Every read validates length
/// before slicing, so a truncated file becomes `Truncated`, never a panic.
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], SnapshotError> {
        let end = self.pos.checked_add(len).ok_or(SnapshotError::Truncated)?;
        if end > self.data.len() {
            return Err(SnapshotError::Truncated);
        }
        let slice = &self.data[self.pos..end];
        self.pos = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, SnapshotError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, SnapshotError> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn u64(&mut self) -> Result<u64, SnapshotError> {
        let bytes = self.take(8)?;
        Ok(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    fn f32(&mut self) -> Result<f32, SnapshotError> {
        Ok(f32::from_bits(self.u32()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Five entities, one despawn (leaves a free index and a bumped
    /// generation), one reuse, both standard columns present.
    fn mixed_world() -> World {
        let mut world = World::new();
        let a = world.spawn();
        world.insert(a, Position { x: 1.0, y: 2.0 });
        world.insert(a, Velocity { vx: 0.5, vy: -0.25 });

        let b = world.spawn();
        world.insert(b, Position { x: 3.5, y: 4.0 });

        let c = world.spawn();
        world.insert(c, Velocity { vx: 0.0, vy: 0.0 });

        world.despawn(b);

        // Reuses b's index with a bumped generation.
        let d = world.spawn();
        world.insert(d, Position { x: 9.0, y: 9.0 });
        world
    }

    #[test]
    fn round_trip_preserves_state_hash_and_len() {
        let original = mixed_world();
        let bytes = original.snapshot();
        let restored = World::from_snapshot(&bytes).expect("restore");

        assert_eq!(original.len(), restored.len());
        assert_eq!(original.state_hash(), restored.state_hash());
        // Spot-check that components came back as values, not just a hash.
        let e = restored.query::<Position>().next().expect("one position");
        assert_eq!(e.1, &Position { x: 1.0, y: 2.0 });
    }

    #[test]
    fn snapshot_bytes_are_deterministic() {
        assert_eq!(mixed_world().snapshot(), mixed_world().snapshot());
    }

    #[test]
    fn stale_handles_stay_rejected_after_restore() {
        let mut world = World::new();
        let old = world.spawn();
        world.despawn(old);
        let fresh = world.spawn();
        // Same slot, newer generation: this is the case that dies if a
        // snapshot drops generations.
        assert_eq!(old.index, fresh.index);
        assert_ne!(old.generation, fresh.generation);

        let mut restored = World::from_snapshot(&world.snapshot()).expect("restore");
        restored.insert(old, Position { x: 1.0, y: 1.0 });
        assert!(
            restored.get::<Position>(old).is_none(),
            "stale handle must not write"
        );
        restored.insert(fresh, Position { x: 2.0, y: 2.0 });
        assert!(
            restored.get::<Position>(fresh).is_some(),
            "live handle must write"
        );
    }

    #[test]
    fn free_list_order_survives_so_spawns_match() {
        let mut world = World::new();
        let a = world.spawn();
        let b = world.spawn();
        world.despawn(a);
        world.despawn(b);

        let mut restored = World::from_snapshot(&world.snapshot()).expect("restore");
        // Free list is LIFO; the restored world must hand out b then a,
        // exactly as the original would.
        assert_eq!(world.spawn(), restored.spawn());
        assert_eq!(world.spawn(), restored.spawn());
    }

    #[test]
    fn nan_and_negative_zero_survive_bit_identical() {
        let mut world = World::new();
        let e = world.spawn();
        world.insert(
            e,
            Position {
                x: f32::NAN,
                y: -0.0,
            },
        );
        let restored = World::from_snapshot(&world.snapshot()).expect("restore");
        assert_eq!(world.state_hash(), restored.state_hash());
        let p = restored.get::<Position>(e).expect("position");
        assert!(p.x.is_nan());
        assert!(p.y == 0.0 && p.y.is_sign_negative());
    }

    #[test]
    fn absent_column_is_absent_after_restore() {
        let mut world = World::new();
        let e = world.spawn();
        world.insert(e, Position { x: 0.0, y: 0.0 });
        let restored = World::from_snapshot(&world.snapshot()).expect("restore");
        assert!(restored.get::<Velocity>(e).is_none());
    }

    #[test]
    fn corrupt_input_is_refused_and_restore_is_all_or_nothing() {
        let bytes = mixed_world().snapshot();

        let mut bad_magic = bytes.clone();
        bad_magic[0] = b'X';
        assert!(matches!(
            World::from_snapshot(&bad_magic),
            Err(SnapshotError::BadMagic)
        ));

        let mut bad_version = bytes.clone();
        bad_version[8..12].copy_from_slice(&99u32.to_le_bytes());
        assert!(matches!(
            World::from_snapshot(&bad_version),
            Err(SnapshotError::UnsupportedVersion(99))
        ));

        let truncated = &bytes[..bytes.len() - 3];
        assert!(matches!(
            World::from_snapshot(truncated),
            Err(SnapshotError::Truncated)
        ));

        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(matches!(
            World::from_snapshot(&trailing),
            Err(SnapshotError::TrailingBytes)
        ));

        // A failed restore must not disturb the live world.
        let mut live = mixed_world();
        let before = live.state_hash();
        assert!(live.restore(&bad_magic).is_err());
        assert_eq!(live.state_hash(), before);
        assert_eq!(live.len(), mixed_world().len());
    }
}
