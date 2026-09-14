use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use elanmoc_proto::SlotState;

/// Mapping file, mode 0600, owned by root.
pub const DEFAULT_PATH: &str = "/var/lib/elanmoc/prints.json";

/// Highest slot id scanned.
pub const MAX_SLOT: u8 = 15;

/// Names fprintd accepts, GNOME requires.
pub const FINGER_NAMES: [&str; 10] = [
    "left-thumb",
    "left-index-finger",
    "left-middle-finger",
    "left-ring-finger",
    "left-little-finger",
    "right-thumb",
    "right-index-finger",
    "right-middle-finger",
    "right-ring-finger",
    "right-little-finger",
];

pub fn is_valid_finger(name: &str) -> bool {
    FINGER_NAMES.contains(&name)
}

/// User to finger to slot.
pub type Prints = BTreeMap<String, BTreeMap<String, u8>>;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("store file does not parse: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("unknown finger name '{0}'")]
    BadFinger(String),
    #[error("slot {0} already maps to {1}/{2}")]
    SlotTaken(u8, String, String),
}

/// A file-backed mapping.
pub struct Store {
    path: PathBuf,
    prints: Prints,
}

impl Store {
    /// Load the file, or start empty when it does not exist yet.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let prints = match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(StoreError::Io(e)),
        };
        Ok(Self {
            path: path.to_path_buf(),
            prints,
        })
    }

    /// Current mapping.
    pub fn prints(&self) -> &Prints {
        &self.prints
    }

    /// Record a new enrollment. Names are checked, slots are unique.
    pub fn insert(&mut self, user: &str, finger: &str, slot: u8) -> Result<(), StoreError> {
        if !is_valid_finger(finger) {
            return Err(StoreError::BadFinger(finger.to_string()));
        }
        for (u, fingers) in &self.prints {
            for (f, s) in fingers {
                if *s == slot && (u != user || f != finger) {
                    return Err(StoreError::SlotTaken(slot, u.clone(), f.clone()));
                }
            }
        }
        self.prints
            .entry(user.to_string())
            .or_default()
            .insert(finger.to_string(), slot);
        Ok(())
    }

    /// Drop one mapping. Missing entries are not an error.
    pub fn remove(&mut self, user: &str, finger: &str) {
        if let Some(fingers) = self.prints.get_mut(user) {
            fingers.remove(finger);
            if fingers.is_empty() {
                self.prints.remove(user);
            }
        }
    }

    /// Save atomically: temp file in the same directory, fsync, rename.
    pub fn save(&self) -> Result<(), StoreError> {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SALT: AtomicU64 = AtomicU64::new(0);
        let dir = self.path.parent().ok_or_else(|| {
            StoreError::Io(io::Error::new(io::ErrorKind::NotFound, "store path has no parent"))
        })?;
        std::fs::create_dir_all(dir)?;
        let n = SALT.fetch_add(1, Ordering::Relaxed);
        let tmp = dir.join(format!(".prints.{}.{n}.tmp", std::process::id()));
        let text = serde_json::to_string_pretty(&self.prints)?;
        {
            use std::os::unix::fs::OpenOptionsExt;
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true).mode(0o600);
            let mut file = opts.open(&tmp)?;
            use std::io::Write;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
        }
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

/// Ids an enrollment may write, lowest first.
///
/// `count` is `enrolled_num`, read from the chip. Slots below it are presumed
/// to hold templates: on 0c90 an occupied slot answers `finger_info` with the
/// same `40 ff` an empty one does, so no scan separates them. The store only
/// removes further candidates, never restores one, so a store that disagrees
/// with the device can never hand out a slot the device says is in use.
pub fn writable_slots(count: u8, tracked: &[u8]) -> Vec<u8> {
    (count..=MAX_SLOT)
        .filter(|id| !tracked.contains(id))
        .collect()
}

/// Slots the store believes are occupied, across every user.
pub fn tracked_slots(prints: &Prints) -> Vec<u8> {
    let mut slots: Vec<u8> = prints
        .values()
        .flat_map(|fingers| fingers.values().copied())
        .collect();
    slots.sort_unstable();
    slots.dedup();
    slots
}

/// One store entry paired with what the device said about its slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryStatus {
    pub user: String,
    pub finger: String,
    pub slot: u8,
    /// True only when proven by the device, never from the 2 byte form alone.
    pub stale: bool,
    /// Human-readable reason.
    pub reason: String,
}

/// Result of comparing the store against the device.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SyncReport {
    pub confirmed: Vec<EntryStatus>,
    pub unconfirmed: Vec<EntryStatus>,
    pub stale: Vec<EntryStatus>,
    pub warnings: Vec<String>,
}

/// Compare the store against `enrolled_num` and a slot scan.
///
/// The 2 byte `40 ff` form proves nothing on its own: on 0c90 an invalid id
/// and an empty slot answer identically. An entry is `stale` only when the
/// device proves it: `enrolled_num` at 0, or a 70 byte record for that slot.
pub fn reconcile(store: &Prints, count: u8, records: &[(u8, SlotState)]) -> SyncReport {
    let mut report = SyncReport::default();
    let by_slot: BTreeMap<u8, &SlotState> =
        records.iter().map(|(id, s)| (*id, s)).collect();

    if count == 0 {
        for (user, fingers) in store {
            for (finger, slot) in fingers {
                report.stale.push(EntryStatus {
                    user: user.clone(),
                    finger: finger.clone(),
                    slot: *slot,
                    stale: true,
                    reason: "device holds nothing, enrolled_num is 0".to_string(),
                });
            }
        }
        return report;
    }

    for (user, fingers) in store {
        for (finger, slot) in fingers {
            let entry = EntryStatus {
                user: user.clone(),
                finger: finger.clone(),
                slot: *slot,
                stale: false,
                reason: String::new(),
            };
            match by_slot.get(slot) {
                Some(SlotState::Enrolled { .. }) => {
                    report.confirmed.push(EntryStatus {
                        reason: "70 byte record present".to_string(),
                        ..entry
                    });
                }
                Some(SlotState::Empty) => {
                    report.stale.push(EntryStatus {
                        stale: true,
                        reason: "70 byte record reads empty".to_string(),
                        ..entry
                    });
                }
                Some(SlotState::Stuck) => {
                    report.unconfirmed.push(EntryStatus {
                        reason: "sensor reports stuck, run verify to clear".to_string(),
                        ..entry
                    });
                }
                Some(SlotState::Error(_)) => {
                    report.unconfirmed.push(EntryStatus {
                        reason: "2 byte form, empty and invalid read alike".to_string(),
                        ..entry
                    });
                }
                None => {
                    report.unconfirmed.push(EntryStatus {
                        reason: "slot outside the scanned range".to_string(),
                        ..entry
                    });
                }
            }
        }
    }

    let claimed = store.values().map(|f| f.len()).sum::<usize>();
    if claimed > usize::from(count) {
        report.warnings.push(format!(
            "store claims {claimed} fingers but the device counts {}, {} proven stale",
            count,
            report.stale.len()
        ));
    }
    for (id, state) in records {
        if matches!(state, SlotState::Enrolled { .. })
            && !store.values().flat_map(|f| f.values()).any(|s| s == id)
        {
            report.warnings.push(format!("slot {id} holds a template the store does not track"));
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("elanmoc-store-{}-{}.json", std::process::id(), tag));
        p
    }

    #[test]
    fn finger_names_match_fprintd() {
        assert!(is_valid_finger("right-index-finger"));
        assert!(!is_valid_finger("Right Index"));
        assert!(!is_valid_finger("right-index"));
    }

    #[test]
    fn insert_rejects_bad_names_and_taken_slots() {
        let mut store = Store {
            path: tmp_path("reject"),
            prints: BTreeMap::new(),
        };
        assert!(store.insert("u", "nope", 1).is_err());
        assert!(store.insert("u", "right-index-finger", 1).is_ok());
        assert!(store.insert("v", "left-thumb", 1).is_err());
        assert!(store.insert("u", "left-thumb", 2).is_ok());
    }

    #[test]
    fn save_and_reload_roundtrip() {
        let path = tmp_path("roundtrip");
        let _ = std::fs::remove_file(&path);
        let mut store = Store {
            path: path.clone(),
            prints: BTreeMap::new(),
        };
        assert!(store.insert("u", "right-index-finger", 3).is_ok());
        assert!(store.save().is_ok());
        let back = match Store::open(&path) {
            Ok(b) => b,
            Err(e) => panic!("reload works: {e}"),
        };
        assert_eq!(back.prints["u"]["right-index-finger"], 3);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn missing_file_opens_empty() {
        let path = tmp_path("missing");
        let _ = std::fs::remove_file(&path);
        let store = match Store::open(&path) {
            Ok(s) => s,
            Err(e) => panic!("missing file opens empty: {e}"),
        };
        assert!(store.prints.is_empty());
    }

    #[test]
    fn empty_device_makes_everything_stale() {
        let mut store: Prints = BTreeMap::new();
        store
            .entry("u".to_string())
            .or_default()
            .insert("right-index-finger".to_string(), 1);
        let records = vec![(1u8, SlotState::Error(0xff))];
        let report = reconcile(&store, 0, &records);
        assert_eq!(report.stale.len(), 1);
        assert!(report.confirmed.is_empty());
    }

    #[test]
    fn two_byte_form_alone_never_proves_stale() {
        let mut store: Prints = BTreeMap::new();
        store
            .entry("u".to_string())
            .or_default()
            .insert("right-index-finger".to_string(), 1);
        store
            .entry("u".to_string())
            .or_default()
            .insert("left-thumb".to_string(), 2);
        let records = vec![
            (1u8, SlotState::Error(0xff)),
            (2u8, SlotState::Error(0xff)),
        ];
        let report = reconcile(&store, 1, &records);
        assert!(report.stale.is_empty());
        assert_eq!(report.unconfirmed.len(), 2);
        assert_eq!(report.warnings.len(), 1);
    }

    #[test]
    fn seventy_byte_record_confirms_and_contradicts() {
        let mut store: Prints = BTreeMap::new();
        store
            .entry("u".to_string())
            .or_default()
            .insert("right-index-finger".to_string(), 1);
        store
            .entry("u".to_string())
            .or_default()
            .insert("left-thumb".to_string(), 2);
        let mut occupied = vec![0u8; 70];
        occupied[69] = 0x01;
        let records = vec![
            (1u8, SlotState::Enrolled { raw: occupied }),
            (2u8, SlotState::Empty),
        ];
        let report = reconcile(&store, 1, &records);
        assert_eq!(report.confirmed.len(), 1);
        assert_eq!(report.stale.len(), 1);
        assert_eq!(report.stale[0].slot, 2);
    }

    #[test]
    fn the_device_count_is_the_floor_not_the_store() {
        assert_eq!(writable_slots(1, &[])[0], 1);
        assert_eq!(writable_slots(0, &[])[0], 0);
        assert_eq!(writable_slots(3, &[])[0], 3);
    }

    #[test]
    fn an_empty_store_never_reopens_a_slot_the_device_counts() {
        for count in 1..=MAX_SLOT {
            assert!(
                !writable_slots(count, &[]).contains(&0),
                "count {count} offered slot 0"
            );
        }
    }

    #[test]
    fn the_store_only_removes_candidates() {
        assert!(!writable_slots(0, &[0, 1]).contains(&0));
        assert!(!writable_slots(0, &[0, 1]).contains(&1));
        assert_eq!(writable_slots(0, &[0, 1])[0], 2);
    }

    #[test]
    fn a_full_device_offers_nothing() {
        assert!(writable_slots(MAX_SLOT + 1, &[]).is_empty());
    }

    #[test]
    fn tracked_slots_are_sorted_and_unique() {
        let mut prints: Prints = BTreeMap::new();
        prints
            .entry("u".to_string())
            .or_default()
            .insert("left-thumb".to_string(), 2);
        prints
            .entry("v".to_string())
            .or_default()
            .insert("right-thumb".to_string(), 2);
        prints
            .entry("v".to_string())
            .or_default()
            .insert("left-index-finger".to_string(), 0);
        assert_eq!(tracked_slots(&prints), vec![0, 2]);
    }

    #[test]
    fn concurrent_saves_leave_a_parseable_file() {
        use std::sync::{Arc, Barrier};
        let path = tmp_path("concurrent");
        let barrier = Arc::new(Barrier::new(4));
        let mut handles = Vec::new();
        for n in 0..4u8 {
            let b = barrier.clone();
            let p = path.clone();
            handles.push(std::thread::spawn(move || {
                b.wait();
                let mut store = Store {
                    path: p,
                    prints: BTreeMap::new(),
                };
                assert!(store.insert("u", "right-index-finger", n).is_ok());
                assert!(store.save().is_ok());
            }));
        }
        for h in handles {
            if h.join().is_err() {
                panic!("saver thread works");
            }
        }
        let back = match Store::open(&path) {
            Ok(b) => b,
            Err(e) => panic!("concurrent saves parse: {e}"),
        };
        assert_eq!(back.prints["u"].len(), 1);
        let _ = std::fs::remove_file(&path);
    }
}
