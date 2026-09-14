//! Host-owned query data and terminal reply encoders.
//!
//! These settings are not part of GHOSTSNP. Hosts reapply them after restore.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorScheme {
    Light,
    Dark,
}

impl ColorScheme {
    pub fn encode(self) -> Vec<u8> {
        match self {
            Self::Light => b"\x1b[?997;2n",
            Self::Dark => b"\x1b[?997;1n",
        }
        .to_vec()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttributeKind {
    Primary,
    Secondary,
    Tertiary,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceAttributes {
    pub conformance_level: u16,
    pub features: Vec<u16>,
    pub device_type: u16,
    pub firmware_version: u16,
    pub rom_cartridge: u16,
    pub unit_id: u32,
}

impl Default for DeviceAttributes {
    fn default() -> Self {
        Self {
            conformance_level: 62,
            features: vec![22],
            device_type: 1,
            firmware_version: 0,
            rom_cartridge: 0,
            unit_id: 0,
        }
    }
}

impl DeviceAttributes {
    pub fn encode(&self, kind: AttributeKind) -> Vec<u8> {
        match kind {
            AttributeKind::Primary => {
                let mut reply = format!("\x1b[?{}", self.conformance_level);
                for feature in &self.features {
                    reply.push_str(&format!(";{feature}"));
                }
                reply.push('c');
                reply.into_bytes()
            }
            AttributeKind::Secondary => format!(
                "\x1b[>{};{};{}c",
                self.device_type, self.firmware_version, self.rom_cartridge
            )
            .into_bytes(),
            AttributeKind::Tertiary => format!("\x1bP!|{:08X}\x1b\\", self.unit_id).into_bytes(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SizeStyle {
    TextPixels,
    CellPixels,
    Cells,
    InBand,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Size {
    pub rows: u16,
    pub columns: u16,
    pub cell_width: u32,
    pub cell_height: u32,
}

impl Size {
    pub fn encode(self, style: SizeStyle) -> Vec<u8> {
        let width = u64::from(self.columns) * u64::from(self.cell_width);
        let height = u64::from(self.rows) * u64::from(self.cell_height);
        match style {
            SizeStyle::TextPixels => format!("\x1b[4;{height};{width}t"),
            SizeStyle::CellPixels => format!("\x1b[6;{};{}t", self.cell_height, self.cell_width),
            SizeStyle::Cells => format!("\x1b[8;{};{}t", self.rows, self.columns),
            SizeStyle::InBand => {
                format!("\x1b[48;{};{};{height};{width}t", self.rows, self.columns)
            }
        }
        .into_bytes()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Query {
    ColorScheme,
    DeviceAttributes(AttributeKind),
    Enquiry,
    Size(SizeStyle),
    Xtversion,
}

/// Runtime query answers used by `Terminal::feed`. Synchronous hosts using
/// `feed_with_handler` supply their own answers through `EffectHandler` instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Defaults {
    pub color_scheme: Option<ColorScheme>,
    /// Actual keyboard focus, when a host wants an initial mode-1004 report.
    pub focused: Option<bool>,
    pub device_attributes: Option<DeviceAttributes>,
    pub enquiry: Vec<u8>,
    pub xtversion: Vec<u8>,
    pub size_reports: bool,
    /// Actual host geometry, independent of DEC's forced 80/132-column grid.
    /// Also restores the host grid when DEC mode 40 changes.
    pub size: Option<Size>,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            color_scheme: None,
            focused: None,
            device_attributes: Some(DeviceAttributes::default()),
            enquiry: Vec::new(),
            xtversion: concat!("ghostty ", env!("CARGO_PKG_VERSION"))
                .as_bytes()
                .to_vec(),
            size_reports: true,
            size: None,
        }
    }
}

impl Defaults {
    pub(crate) fn reply(&self, query: Query, size: Size) -> Option<Vec<u8>> {
        match query {
            Query::ColorScheme => self.color_scheme.map(ColorScheme::encode),
            Query::DeviceAttributes(kind) => {
                self.device_attributes.as_ref().map(|a| a.encode(kind))
            }
            Query::Enquiry => enquiry(&self.enquiry),
            Query::Size(style) => self
                .size_reports
                .then(|| self.size.unwrap_or(size).encode(style)),
            Query::Xtversion => xtversion(&self.xtversion),
        }
    }
}

pub(crate) fn enquiry(bytes: &[u8]) -> Option<Vec<u8>> {
    (!bytes.is_empty() && bytes.len() < 256).then(|| bytes.to_vec())
}

pub(crate) fn xtversion(bytes: &[u8]) -> Option<Vec<u8>> {
    let bytes = if bytes.is_empty() {
        b"libghostty"
    } else {
        bytes
    };
    if bytes.len() > 281 {
        return None;
    }
    let mut reply = b"\x1bP>|".to_vec();
    reply.extend_from_slice(bytes);
    reply.extend_from_slice(b"\x1b\\");
    Some(reply)
}

pub fn visibility(visible: bool) -> Vec<u8> {
    if visible {
        b"\x1b[?999;1n"
    } else {
        b"\x1b[?999;2n"
    }
    .to_vec()
}
