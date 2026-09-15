use serde::{Deserialize, Serialize};

/// Mode numbers follow ANSI/DEC; the boolean is true for DEC's `?` prefix.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Modes {
    #[serde(with = "mode_map")]
    values: u64,
    #[serde(with = "mode_map")]
    saved: u64,
    #[serde(with = "mode_map")]
    defaults: u64,
}

const ANSI: &[u16] = &[2, 4, 12, 20];
const DEC: &[u16] = &[
    1, 3, 4, 5, 6, 7, 8, 9, 12, 25, 40, 45, 47, 66, 67, 69, 1000, 1002, 1003, 1004, 1005, 1006,
    1007, 1015, 1016, 1035, 1036, 1039, 1045, 1047, 1048, 1049, 2004, 2026, 2027, 2031, 2033, 2048,
    5522,
];

// The order is also the stable snapshot bit order.
const ALL: u64 = (1 << (ANSI.len() + DEC.len())) - 1;
const INITIAL: u64 = bit(false, 12)
    | bit(true, 7)
    | bit(true, 25)
    | bit(true, 1007)
    | bit(true, 1035)
    | bit(true, 1036);

fn keys() -> impl Iterator<Item = (bool, u16)> {
    ANSI.iter()
        .map(|&n| (false, n))
        .chain(DEC.iter().map(|&n| (true, n)))
}

// Inline so constant mode numbers in printing and input become a single bit test.
#[inline(always)]
const fn bit(private: bool, mode: u16) -> u64 {
    let modes = if private { DEC } else { ANSI };
    let mut i = 0;
    while i < modes.len() {
        if modes[i] == mode {
            return 1 << (i + if private { ANSI.len() } else { 0 });
        }
        i += 1;
    }
    0
}

// Preserve the public serde map shape; snapshots use packed() directly.
mod mode_map {
    use super::*;
    use serde::{Deserializer, Serializer, de::Error};
    use std::collections::BTreeMap;

    pub fn serialize<S: Serializer>(bits: &u64, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_map(
            keys()
                .enumerate()
                .map(|(i, key)| (key, bits & (1 << i) != 0)),
        )
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
        let values = BTreeMap::<(bool, u16), bool>::deserialize(deserializer)?;
        let mut seen = 0;
        let mut bits = 0;
        for ((private, mode), value) in values {
            let mask = bit(private, mode);
            if mask == 0 {
                return Err(D::Error::custom("unsupported terminal mode"));
            }
            seen |= mask;
            if value {
                bits |= mask;
            }
        }
        if seen != ALL {
            return Err(D::Error::custom("missing terminal mode"));
        }
        Ok(bits)
    }
}

impl Default for Modes {
    fn default() -> Self {
        Self {
            values: INITIAL,
            saved: INITIAL,
            defaults: INITIAL,
        }
    }
}

impl Modes {
    /// Native formatter order is ANSI first, then DEC, ascending by number.
    pub(crate) fn changed(&self) -> impl Iterator<Item = ((bool, u16), bool)> + '_ {
        keys().enumerate().filter_map(|(i, key)| {
            let mask = 1 << i;
            ((self.values ^ self.defaults) & mask != 0).then_some((key, self.values & mask != 0))
        })
    }

    pub(crate) fn packed(&self) -> [u64; 3] {
        [self.values, self.saved, self.defaults]
    }

    pub(crate) fn from_packed(bits: [u64; 3]) -> Self {
        let [values, saved, defaults] = bits.map(|bits| bits & ALL);
        Self {
            values,
            saved,
            defaults,
        }
    }
    #[inline]
    pub fn get(&self, private: bool, mode: u16) -> bool {
        self.values & bit(private, mode) != 0
    }
    #[inline]
    pub fn dec(&self, mode: u16) -> bool {
        self.get(true, mode)
    }
    pub fn get_saved(&self, private: bool, mode: u16) -> Option<bool> {
        let mask = bit(private, mode);
        (mask != 0).then_some(self.saved & mask != 0)
    }
    pub fn get_default(&self, private: bool, mode: u16) -> Option<bool> {
        let mask = bit(private, mode);
        (mask != 0).then_some(self.defaults & mask != 0)
    }
    /// Whether a host can configure this reset default without performing a
    /// transition or updating state outside the mode bit.
    pub fn default_configurable(private: bool, mode: u16) -> bool {
        if !private {
            return ANSI.contains(&mode);
        }
        DEC.contains(&mode)
            && ![
                3, 6, 9, 12, 47, 69, 1000, 1002, 1003, 1005, 1006, 1015, 1016, 1047, 1048, 1049,
                2026, 2033,
            ]
            .contains(&mode)
    }
    pub fn set(&mut self, private: bool, mode: u16, value: bool) -> bool {
        let mask = bit(private, mode);
        self.values = (self.values & !mask) | if value { mask } else { 0 };
        mask != 0
    }
    pub fn save(&mut self, private: bool, mode: u16) {
        let mask = bit(private, mode);
        self.saved = (self.saved & !mask) | (self.values & mask);
    }
    pub fn restore(&mut self, private: bool, mode: u16) -> bool {
        let mask = bit(private, mode);
        self.values = (self.values & !mask) | (self.saved & mask);
        self.values & mask != 0
    }
    /// Set the raw mode bit's current value and reset default. This does not
    /// perform mode transitions; hosts should use `Terminal::set_default_mode`.
    pub fn set_default(&mut self, private: bool, mode: u16, value: bool) -> bool {
        if !self.set(private, mode, value) {
            return false;
        }
        let mask = bit(private, mode);
        self.defaults = (self.defaults & !mask) | (self.values & mask);
        true
    }
    pub fn reset(&mut self) {
        self.values = self.defaults;
        self.saved = INITIAL;
    }
    pub fn report(&self, private: bool, mode: u16) -> u8 {
        // A native ModeTag stores its number in fifteen bits.
        let mode = mode & 0x7fff;
        if private && mode == 117 {
            return 4;
        }
        let mask = bit(private, mode);
        if mask == 0 {
            0
        } else if self.values & mask != 0 {
            1
        } else {
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed_modes_preserve_every_snapshot_bit_and_mask_unknown_bits() {
        for (i, (private, mode)) in keys().enumerate() {
            let mask = 1 << i;
            let mut modes = Modes::from_packed([mask, ALL ^ mask, mask]);
            assert!(modes.get(private, mode));
            assert_eq!(modes.get_saved(private, mode), Some(false));
            assert_eq!(modes.get_default(private, mode), Some(true));
            assert!(!modes.restore(private, mode));
            assert_eq!(modes.packed(), [0, ALL ^ mask, mask]);
            modes.reset();
            assert_eq!(modes.packed(), [mask, INITIAL, mask]);
            assert_eq!(modes.changed().count(), 0);
            modes.set(private, mode, false);
            assert_eq!(
                modes.changed().collect::<Vec<_>>(),
                [((private, mode), false)]
            );
        }
        assert_eq!(Modes::from_packed([u64::MAX; 3]).packed(), [ALL; 3]);
    }

    #[test]
    fn serde_mode_maps_require_the_supported_keys() {
        use serde::de::value::MapDeserializer;

        let entries: Vec<_> = keys()
            .map(|(private, mode)| (serde_json::json!([private, mode]), mode == 7))
            .collect();
        let decode = |entries: Vec<(serde_json::Value, bool)>| {
            mode_map::deserialize(MapDeserializer::<_, serde_json::Error>::new(
                entries.into_iter(),
            ))
        };
        assert_eq!(decode(entries.clone()).unwrap(), bit(true, 7));
        let mut missing = entries.clone();
        missing.pop();
        assert!(decode(missing).unwrap_err().to_string().contains("missing"));
        let mut unknown = entries;
        unknown.push((serde_json::json!([true, 999]), true));
        assert!(
            decode(unknown)
                .unwrap_err()
                .to_string()
                .contains("unsupported")
        );
    }
}
