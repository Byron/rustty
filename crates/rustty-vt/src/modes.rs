use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Mode numbers follow ANSI/DEC; the boolean is true for DEC's `?` prefix.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Modes {
    values: BTreeMap<(bool, u16), bool>,
    saved: BTreeMap<(bool, u16), bool>,
    defaults: BTreeMap<(bool, u16), bool>,
}

const ANSI: &[u16] = &[2, 4, 12, 20];
const DEC: &[u16] = &[
    1, 3, 4, 5, 6, 7, 8, 9, 12, 25, 40, 45, 47, 66, 67, 69, 1000, 1002, 1003, 1004, 1005, 1006,
    1007, 1015, 1016, 1035, 1036, 1039, 1045, 1047, 1048, 1049, 2004, 2026, 2027, 2031, 2033, 2048,
    5522,
];

impl Default for Modes {
    fn default() -> Self {
        let mut values = BTreeMap::new();
        for &mode in ANSI {
            values.insert((false, mode), mode == 12);
        }
        for &mode in DEC {
            values.insert((true, mode), matches!(mode, 7 | 25 | 1007 | 1035 | 1036));
        }
        Self {
            defaults: values.clone(),
            saved: values.clone(),
            values,
        }
    }
}

impl Modes {
    /// Native formatter order is ANSI first, then DEC, ascending by number.
    pub(crate) fn changed(&self) -> impl Iterator<Item = ((bool, u16), bool)> + '_ {
        self.values.iter().filter_map(|(&key, &value)| {
            (self.defaults.get(&key) != Some(&value)).then_some((key, value))
        })
    }

    pub(crate) fn packed(&self) -> [u64; 3] {
        [&self.values, &self.saved, &self.defaults].map(|values| {
            ANSI.iter()
                .map(|&n| (false, n))
                .chain(DEC.iter().map(|&n| (true, n)))
                .enumerate()
                .fold(0u64, |bits, (i, key)| {
                    bits | (u64::from(
                        values
                            .get(&key)
                            .copied()
                            .unwrap_or_else(|| self.defaults.get(&key).copied().unwrap_or(false)),
                    ) << i)
                })
        })
    }

    pub(crate) fn from_packed(bits: [u64; 3]) -> Self {
        let [values, saved, defaults] = bits.map(|bits| {
            ANSI.iter()
                .map(|&n| (false, n))
                .chain(DEC.iter().map(|&n| (true, n)))
                .enumerate()
                .map(|(i, key)| (key, bits & (1 << i) != 0))
                .collect()
        });
        Self {
            values,
            saved,
            defaults,
        }
    }
    pub fn get(&self, private: bool, mode: u16) -> bool {
        self.values.get(&(private, mode)).copied().unwrap_or(false)
    }
    pub fn dec(&self, mode: u16) -> bool {
        self.get(true, mode)
    }
    pub fn get_saved(&self, private: bool, mode: u16) -> Option<bool> {
        self.saved.get(&(private, mode)).copied()
    }
    pub fn get_default(&self, private: bool, mode: u16) -> Option<bool> {
        self.defaults.get(&(private, mode)).copied()
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
        if let Some(current) = self.values.get_mut(&(private, mode)) {
            *current = value;
            true
        } else {
            false
        }
    }
    pub fn save(&mut self, private: bool, mode: u16) {
        if let Some(&value) = self.values.get(&(private, mode)) {
            self.saved.insert((private, mode), value);
        }
    }
    pub fn restore(&mut self, private: bool, mode: u16) -> bool {
        let value = self.saved.get(&(private, mode)).copied().unwrap_or(false);
        self.set(private, mode, value);
        value
    }
    /// Set the raw mode bit's current value and reset default. This does not
    /// perform mode transitions; hosts should use `Terminal::set_default_mode`.
    pub fn set_default(&mut self, private: bool, mode: u16, value: bool) -> bool {
        if !self.set(private, mode, value) {
            return false;
        }
        self.defaults.insert((private, mode), value);
        true
    }
    pub fn reset(&mut self) {
        self.values.clone_from(&self.defaults);
        self.saved = Self::default().saved;
    }
    pub fn report(&self, private: bool, mode: u16) -> u8 {
        // A native ModeTag stores its number in fifteen bits.
        let mode = mode & 0x7fff;
        if private && mode == 117 {
            return 4;
        }
        self.values
            .get(&(private, mode))
            .map_or(0, |v| if *v { 1 } else { 2 })
    }
}
