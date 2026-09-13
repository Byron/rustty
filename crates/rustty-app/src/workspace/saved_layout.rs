//! Read-only layout import. Native archives are decoded as data, never as views.
use super::{Axis, Id, Node, SavedPane, Tab, Tree, WindowState, Workspace, invalid};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

const MAX_LAYOUT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct SavedLayout {
    pub name: String,
    pub path: PathBuf,
    pub available: bool,
}

/// Include unavailable defaults so the picker can explain where layouts live.
pub fn saved_layouts(home: &Path, rustty_state_path: &Path) -> Vec<SavedLayout> {
    let mut layouts = [
        ("Ghostty Local", "com.mitchellh.ghostty.local"),
        ("Ghostty", "com.mitchellh.ghostty"),
    ]
    .into_iter()
    .map(|(name, bundle)| {
        let classic = home
            .join("Library/Saved Application State")
            .join(format!("{bundle}.savedState"));
        let mut candidates = vec![classic.clone()];
        candidates.push(
            home.join("Library/Containers")
                .join(bundle)
                .join("Data/Library/Saved Application State")
                .join(format!("{bundle}.savedState")),
        );
        #[cfg(target_os = "macos")]
        candidates.extend(native::mapped_layouts(home, bundle));
        let path = candidates
            .into_iter()
            .filter(|path| native_layout_exists(path))
            .max_by_key(|path| {
                fs::metadata(path.join("data.data"))
                    .and_then(|m| m.modified())
                    .ok()
            })
            .unwrap_or(classic);
        SavedLayout {
            name: name.into(),
            available: native_layout_exists(&path),
            path,
        }
    })
    .collect::<Vec<_>>();
    layouts.push(SavedLayout {
        name: "Rustty".into(),
        path: rustty_state_path.into(),
        available: rustty_state_path.is_file(),
    });
    layouts
}

fn native_layout_exists(path: &Path) -> bool {
    path.join("windows.plist").is_file() && path.join("data.data").is_file()
}

/// Accept Rustty JSON or a Ghostty saved-state directory (or a file inside it).
/// Loading never creates sessions, saves preferences, or writes to the source.
pub fn load_layout(path: &Path) -> io::Result<Workspace> {
    let directory = if path.is_dir() {
        Some(path)
    } else if matches!(
        path.file_name().and_then(|s| s.to_str()),
        Some("windows.plist" | "data.data")
    ) {
        path.parent()
    } else {
        None
    };
    let state = if let Some(directory) = directory {
        #[cfg(target_os = "macos")]
        {
            native::load(directory)?
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = directory;
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Ghostty archives currently require macOS",
            ));
        }
    } else {
        Workspace::load(path)?
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "saved layout does not exist"))?
    };
    state.validate()?;
    if state.windows.is_empty() {
        return Err(invalid("saved layout contains no terminal windows"));
    }
    Ok(state)
}

fn read_bounded(path: &Path) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(MAX_LAYOUT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_LAYOUT_BYTES {
        return Err(invalid("saved layout file exceeds 8 MiB"));
    }
    Ok(bytes)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhosttyState {
    focused_surface: Option<String>,
    surface_tree: GhosttyTree,
    title_override: Option<String>,
    tab_color: Option<u8>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhosttyTree {
    version: u32,
    root: Option<GhosttyNode>,
    zoomed: Option<GhosttyPath>,
    quadrant_zoomed: Option<GhosttyPath>,
}

#[derive(Deserialize)]
struct GhosttyPath {
    path: Vec<BTreeMap<String, serde_json::Value>>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum GhosttyNode {
    View { view: GhosttyView },
    Split { split: GhosttySplit },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhosttyView {
    uuid: String,
    pwd: Option<PathBuf>,
    title: Option<String>,
    #[serde(default)]
    is_user_set_title: bool,
}

#[derive(Deserialize)]
struct GhosttySplit {
    direction: BTreeMap<String, serde_json::Value>,
    ratio: f32,
    left: Box<GhosttyNode>,
    right: Box<GhosttyNode>,
}

impl GhosttyState {
    fn into_tab(self, workspace: &mut Workspace) -> io::Result<Tab> {
        if self.surface_tree.version != 1 {
            return Err(invalid("unsupported Ghostty split tree version"));
        }
        let id = workspace.id();
        let mut panes = BTreeMap::new();
        let mut uuids = BTreeMap::new();
        fn node(
            source: GhosttyNode,
            workspace: &mut Workspace,
            panes: &mut BTreeMap<Id, SavedPane>,
            uuids: &mut BTreeMap<String, Id>,
            depth: usize,
        ) -> io::Result<Tree> {
            if depth > 64 || panes.len() >= 4096 {
                return Err(invalid("Ghostty split tree is too large"));
            }
            let id = workspace.id();
            Ok(match source {
                GhosttyNode::View { view } => {
                    if view.uuid.is_empty() || uuids.insert(view.uuid, id).is_some() {
                        return Err(invalid(
                            "Ghostty split tree has duplicate surface identifiers",
                        ));
                    }
                    panes.insert(
                        id,
                        SavedPane {
                            working_directory: view.pwd.unwrap_or_default(),
                            title_override: view
                                .is_user_set_title
                                .then(|| view.title.clone())
                                .flatten(),
                            title: view.title,
                        },
                    );
                    Tree::leaf(id)
                }
                GhosttyNode::Split { split } => {
                    let axis = match split
                        .direction
                        .keys()
                        .map(String::as_str)
                        .collect::<Vec<_>>()
                        .as_slice()
                    {
                        ["horizontal"] => Axis::Horizontal,
                        ["vertical"] => Axis::Vertical,
                        _ => return Err(invalid("invalid Ghostty split direction")),
                    };
                    if !split.ratio.is_finite() || !(0.01..=0.99).contains(&split.ratio) {
                        return Err(invalid("invalid Ghostty split ratio"));
                    }
                    Tree {
                        id,
                        kind: Node::Split {
                            axis,
                            ratio: split.ratio,
                            first: Box::new(node(*split.left, workspace, panes, uuids, depth + 1)?),
                            second: Box::new(node(
                                *split.right,
                                workspace,
                                panes,
                                uuids,
                                depth + 1,
                            )?),
                        },
                    }
                }
            })
        }
        let root = node(
            self.surface_tree
                .root
                .ok_or_else(|| invalid("empty Ghostty split tree"))?,
            workspace,
            &mut panes,
            &mut uuids,
            0,
        )?;
        let focused = self
            .focused_surface
            .as_ref()
            .and_then(|uuid| uuids.get(uuid))
            .copied()
            .unwrap_or_else(|| root.panes()[0]);
        let resolve = |path: GhosttyPath| -> io::Result<Id> {
            let mut current = &root;
            for component in path.path {
                let Node::Split { first, second, .. } = &current.kind else {
                    return Err(invalid("Ghostty zoom path leaves its split tree"));
                };
                current = match component
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .as_slice()
                {
                    ["left"] => first,
                    ["right"] => second,
                    _ => return Err(invalid("invalid Ghostty zoom path")),
                };
            }
            Ok(current.id)
        };
        let zoom = self.surface_tree.zoomed.map(resolve).transpose()?;
        let quadrant_zoom = self.surface_tree.quadrant_zoomed.map(resolve).transpose()?;
        // Ghostty's named system colors, represented in Rustty's saved RGB form.
        let color = match self.tab_color {
            None | Some(0) => None,
            Some(1) => Some([0, 122, 255]),
            Some(2) => Some([175, 82, 222]),
            Some(3) => Some([255, 45, 85]),
            Some(4) => Some([255, 59, 48]),
            Some(5) => Some([255, 149, 0]),
            Some(6) => Some([255, 204, 0]),
            Some(7) => Some([52, 199, 89]),
            Some(8) => Some([0, 199, 190]),
            Some(9) => Some([142, 142, 147]),
            _ => return Err(invalid("unknown Ghostty tab color")),
        };
        Ok(Tab {
            id,
            title: self.title_override,
            color,
            root,
            focused,
            zoom,
            quadrant_zoom,
            remembered: BTreeMap::new(),
            panes,
        })
    }
}

#[cfg(target_os = "macos")]
mod native {
    use super::*;
    use objc2::{AnyThread, ClassType, rc::Retained, runtime::AnyObject};
    use objc2_foundation::{
        NSArray, NSData, NSDictionary, NSJSONSerialization, NSJSONWritingOptions,
        NSKeyedUnarchiver, NSNull, NSNumber, NSPropertyListReadOptions,
        NSPropertyListSerialization, NSSet, NSString,
    };
    use std::ffi::c_void;

    fn plist(bytes: &[u8]) -> io::Result<Retained<AnyObject>> {
        // Foundation parses property-list values only; it does not instantiate
        // any archived application classes.
        unsafe {
            NSPropertyListSerialization::propertyListWithData_options_format_error(
                &NSData::with_bytes(bytes),
                NSPropertyListReadOptions::empty(),
                std::ptr::null_mut(),
            )
        }
        .map_err(|_| invalid("invalid macOS property list"))
    }

    fn get(object: &AnyObject, key: &str) -> Option<Retained<AnyObject>> {
        object
            .downcast_ref::<NSDictionary>()?
            .objectForKey(&NSString::from_str(key))
    }

    fn string(object: &AnyObject, key: &str) -> Option<String> {
        Some(get(object, key)?.downcast_ref::<NSString>()?.to_string())
    }

    pub(super) fn mapped_layouts(home: &Path, bundle: &str) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        let Ok(containers) = fs::read_dir(home.join("Library/Daemon Containers")) else {
            return paths;
        };
        for container in containers.flatten().take(1024) {
            let path = container.path();
            let Ok(metadata) =
                read_bounded(&path.join(".com.apple.containermanagerd.metadata.plist"))
                    .and_then(|bytes| plist(&bytes))
            else {
                continue;
            };
            if string(&metadata, "MCMMetadataIdentifier").as_deref() != Some("com.apple.talagent") {
                continue;
            }
            let root = path.join("Data/Library/Saved Application State");
            let Ok(mapping) = read_bounded(&root.join("ApplicationMapping.plist"))
                .and_then(|bytes| plist(&bytes))
            else {
                continue;
            };
            let Some(array) = mapping.downcast_ref::<NSArray>() else {
                continue;
            };
            let entries = array.to_vec();
            for pair in entries.as_chunks::<2>().0 {
                let Some(identity) = get(&pair[0], "protected") else {
                    continue;
                };
                if string(&identity, "signingIdentifier").as_deref() != Some(bundle) {
                    continue;
                }
                let Some(uuid) = pair[1].downcast_ref::<NSString>() else {
                    continue;
                };
                let uuid = uuid.to_string();
                if uuid.len() == 36 && uuid.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
                    paths.push(root.join(format!("{uuid}.savedState")));
                }
            }
        }
        paths
    }

    #[link(name = "System")]
    unsafe extern "C" {
        fn CCCrypt(
            operation: u32,
            algorithm: u32,
            options: u32,
            key: *const c_void,
            key_length: usize,
            iv: *const c_void,
            input: *const c_void,
            input_length: usize,
            output: *mut c_void,
            output_capacity: usize,
            output_length: *mut usize,
        ) -> i32;
    }

    fn decrypt(bytes: &[u8], key: &[u8]) -> io::Result<Vec<u8>> {
        if key.len() != 16 || bytes.is_empty() || !bytes.len().is_multiple_of(16) {
            return Err(invalid("invalid macOS restoration encryption record"));
        }
        let mut output = vec![0; bytes.len()];
        let mut written = 0;
        // AppKit uses AES-128 CBC with a zero IV and explicit archive lengths;
        // bytes beyond that length are not PKCS#7 padding.
        let status = unsafe {
            CCCrypt(
                1,
                0,
                0,
                key.as_ptr().cast(),
                key.len(),
                std::ptr::null(),
                bytes.as_ptr().cast(),
                bytes.len(),
                output.as_mut_ptr().cast(),
                output.len(),
                &mut written,
            )
        };
        if status != 0 || written != output.len() {
            return Err(invalid("could not decrypt macOS restoration record"));
        }
        Ok(output)
    }

    fn u32_at(bytes: &[u8], offset: usize) -> io::Result<u32> {
        let bytes = bytes
            .get(offset..offset + 4)
            .ok_or_else(|| invalid("truncated macOS restoration record"))?;
        Ok(u32::from_be_bytes(bytes.try_into().unwrap()))
    }

    fn unarchiver(bytes: &[u8]) -> io::Result<Retained<NSKeyedUnarchiver>> {
        // This initializer enables secure coding and returns decoding errors
        // instead of raising exceptions. Only selected Foundation values below
        // are decoded; Ghostty view/controller classes are never instantiated.
        unsafe {
            NSKeyedUnarchiver::initForReadingFromData_error(
                NSKeyedUnarchiver::alloc(),
                &NSData::with_bytes(bytes),
            )
        }
        .map_err(|_| invalid("invalid macOS keyed archive"))
    }

    fn decoded_string(archive: &NSKeyedUnarchiver, key: &str) -> Option<String> {
        let key = NSString::from_str(key);
        unsafe {
            if !archive.containsValueForKey(&key) {
                return None;
            }
            archive
                .decodeObjectOfClass_forKey(NSString::class(), &key)?
                .downcast_ref::<NSString>()
                .map(ToString::to_string)
        }
    }

    fn bound_json(object: &AnyObject) -> io::Result<()> {
        // Keyed archives may share containers. JSON expands every reference,
        // so charge every visit rather than just each distinct object. A depth
        // bound also rejects cycles before Foundation's recursive conversion.
        fn visit(
            object: &AnyObject,
            remaining: &mut usize,
            nodes: &mut usize,
            depth: usize,
        ) -> io::Result<()> {
            let exceeded = || invalid("Ghostty layout expands beyond the supported size or depth");
            if depth > 64 {
                return Err(exceeded());
            }
            *nodes = nodes.checked_sub(1).ok_or_else(exceeded)?;
            *remaining = remaining.checked_sub(16).ok_or_else(exceeded)?;
            if let Some(string) = object.downcast_ref::<NSString>() {
                // JSON can escape a UTF-16 code unit as six ASCII bytes.
                let length = string.length().checked_mul(6).ok_or_else(exceeded)?;
                *remaining = remaining.checked_sub(length).ok_or_else(exceeded)?;
            } else if let Some(array) = object.downcast_ref::<NSArray>() {
                if array.count() > *remaining / 16 {
                    return Err(exceeded());
                }
                for item in array {
                    visit(&item, remaining, nodes, depth + 1)?;
                }
            } else if let Some(dict) = object.downcast_ref::<NSDictionary>() {
                if dict.count() > *remaining / 32 {
                    return Err(exceeded());
                }
                for key in &*dict.allKeys() {
                    visit(&key, remaining, nodes, depth + 1)?;
                    let value = dict.objectForKey(&key).ok_or_else(exceeded)?;
                    visit(&value, remaining, nodes, depth + 1)?;
                }
            } else if object.downcast_ref::<NSNumber>().is_some() {
                *remaining = remaining.checked_sub(32).ok_or_else(exceeded)?;
            } else if object.downcast_ref::<NSNull>().is_none() {
                return Err(invalid("unsupported Ghostty layout value"));
            }
            Ok(())
        }
        let mut remaining = MAX_LAYOUT_BYTES;
        let mut nodes = 131_072;
        visit(object, &mut remaining, &mut nodes, 0)
    }

    struct NativeWindow {
        state: GhosttyState,
        group: Option<String>,
        order: usize,
        selected: bool,
        frame: [f64; 4],
    }

    fn decode_window(bytes: &[u8]) -> io::Result<NativeWindow> {
        let archive = unarchiver(bytes)?;
        if decoded_string(&archive, "NSUIID").as_deref() != Some("TerminalWindowRestoration") {
            return Err(invalid("archive is not a Ghostty terminal window"));
        }
        let version = unsafe { archive.decodeIntegerForKey(&NSString::from_str("version")) };
        if !(5..=7).contains(&version) {
            return Err(invalid("unsupported Ghostty window restoration version"));
        }
        let outer = plist(bytes)?;
        let objects = get(&outer, "$objects").ok_or_else(|| invalid("missing archived objects"))?;
        let objects = objects
            .downcast_ref::<NSArray>()
            .ok_or_else(|| invalid("invalid archived objects"))?;
        let classes = NSSet::from_slice(&[
            NSDictionary::<AnyObject, AnyObject>::class(),
            NSArray::<AnyObject>::class(),
            NSString::class(),
            NSNumber::class(),
            NSNull::class(),
        ]);
        let mut state = None;
        for object in objects {
            let data = object
                .downcast_ref::<NSData>()
                .map(NSData::to_vec)
                .or_else(|| {
                    get(&object, "NS.data")?
                        .downcast_ref::<NSData>()
                        .map(NSData::to_vec)
                });
            let Some(data) = data else {
                continue;
            };
            if !data.starts_with(b"bplist00") {
                continue;
            }
            let inner = unarchiver(&data)?;
            if !unsafe { inner.containsValueForKey(&NSString::from_str("value")) } {
                continue;
            }
            let value = unsafe {
                inner.decodeObjectOfClasses_forKey(Some(&classes), &NSString::from_str("value"))
            }
            .ok_or_else(|| invalid("invalid Ghostty layout payload"))?;
            bound_json(&value)?;
            if !unsafe { NSJSONSerialization::isValidJSONObject(&value) } {
                return Err(invalid("invalid Ghostty layout values"));
            }
            let json = unsafe {
                NSJSONSerialization::dataWithJSONObject_options_error(
                    &value,
                    NSJSONWritingOptions::empty(),
                )
            }
            .map_err(|_| invalid("invalid Ghostty layout values"))?;
            let decoded: GhosttyState = serde_json::from_slice(&json.to_vec())
                .map_err(|_| invalid("invalid Ghostty split tree"))?;
            if state.replace(decoded).is_some() {
                return Err(invalid("ambiguous Ghostty layout payload"));
            }
        }
        let state = state.ok_or_else(|| invalid("Ghostty layout payload is missing"))?;
        let frame = decoded_string(&archive, "NSWindowFrame")
            .ok_or_else(|| invalid("missing Ghostty window frame"))?;
        let coordinates = frame
            .split(|c: char| c.is_whitespace() || matches!(c, '{' | '}' | ','))
            .filter(|part| !part.is_empty())
            .map(str::parse::<f64>)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| invalid("invalid Ghostty window frame"))?;
        // NSWindow's frame string can append the saved screen's four bounds.
        if !matches!(coordinates.len(), 4 | 8) {
            return Err(invalid("invalid Ghostty window frame"));
        }
        let mut frame: [f64; 4] = coordinates[..4].try_into().unwrap();
        // AppKit's origin is bottom-left; Winit's is top-left in logical units.
        let display = objc2_core_graphics::CGDisplayBounds(objc2_core_graphics::CGMainDisplayID());
        frame[1] = display.size.height - frame[1] - frame[3];
        let group = decoded_string(&archive, "NSTabGrpID");
        let order = unsafe {
            let key = NSString::from_str("NSTabIdx");
            if archive.containsValueForKey(&key) {
                archive.decodeIntegerForKey(&key).max(0) as usize
            } else {
                0
            }
        };
        let selected = ["NSIsKey", "NSIsMain"].into_iter().any(|key| unsafe {
            let key = NSString::from_str(key);
            archive.containsValueForKey(&key) && archive.decodeBoolForKey(&key)
        });
        if unsafe { archive.error() }.is_some() {
            return Err(invalid("could not decode Ghostty window metadata"));
        }
        Ok(NativeWindow {
            state,
            group,
            order,
            selected,
            frame,
        })
    }

    pub(super) fn load(directory: &Path) -> io::Result<Workspace> {
        let metadata = plist(&read_bounded(&directory.join("windows.plist"))?)?;
        let windows = metadata
            .downcast_ref::<NSArray>()
            .ok_or_else(|| invalid("invalid macOS saved window list"))?;
        let mut z_order = None;
        for window in windows {
            if let Some(order) = get(&window, "NSWindowZOrder")
                && let Some(order) = order.downcast_ref::<NSArray>()
            {
                z_order = Some(
                    order
                        .iter()
                        .filter_map(|number| {
                            number
                                .downcast_ref::<NSNumber>()
                                .map(NSNumber::unsignedIntValue)
                        })
                        .collect::<std::collections::HashSet<_>>(),
                );
            }
        }
        let mut keys = BTreeMap::new();
        for window in windows {
            if string(&window, "NSUIID").as_deref() != Some("TerminalWindowRestoration") {
                continue;
            }
            let id = get(&window, "NSWindowID")
                .and_then(|v| v.downcast_ref::<NSNumber>().map(|n| n.unsignedIntValue()))
                .ok_or_else(|| invalid("missing macOS saved window ID"))?;
            let key = get(&window, "NSDataKey")
                .and_then(|v| v.downcast_ref::<NSData>().map(NSData::to_vec))
                .ok_or_else(|| invalid("missing macOS saved window key"))?;
            // Z-order lists NSWindowNumber (not the restoration record ID).
            // It includes selected tabs in background groups as well as the
            // application's key/main window.
            let selected = z_order.as_ref().and_then(|order| {
                let number = get(&window, "NSWindowNumber")?;
                Some(order.contains(&number.downcast_ref::<NSNumber>()?.unsignedIntValue()))
            });
            if keys.insert(id, (key, selected)).is_some() {
                return Err(invalid("duplicate macOS saved window ID"));
            }
        }
        let data = read_bounded(&directory.join("data.data"))?;
        let mut remaining = data.as_slice();
        let mut latest = BTreeMap::new();
        while !remaining.is_empty() {
            if remaining.get(..8) != Some(b"NSCR1000") {
                return Err(invalid("unsupported macOS restoration record"));
            }
            let id = u32_at(remaining, 8)?;
            let length = u32_at(remaining, 12)? as usize;
            if length <= 16 || length > remaining.len() {
                return Err(invalid("truncated macOS restoration record"));
            }
            if let Some((key, _)) = keys.get(&id) {
                let plain = decrypt(&remaining[16..length], key)?;
                let name_length = u32_at(&plain, 4)? as usize;
                if name_length > plain.len().saturating_sub(16) {
                    return Err(invalid("invalid macOS restoration record name"));
                }
                if plain.get(8..8 + name_length) == Some(b"_NSWindow") {
                    if plain.get(8 + name_length..12 + name_length) != Some(b"rchv") {
                        return Err(invalid("unsupported macOS restoration payload"));
                    }
                    let length = u32_at(&plain, 12 + name_length)? as usize;
                    let payload = plain
                        .get(16 + name_length..16 + name_length + length)
                        .ok_or_else(|| invalid("truncated macOS restoration payload"))?;
                    latest.insert(id, payload.to_vec());
                }
            }
            remaining = &remaining[length..];
        }
        if keys.len() != latest.len() {
            return Err(invalid("saved layout is missing a terminal window record"));
        }
        let mut workspace = Workspace::default();
        let mut groups = BTreeMap::<String, Vec<(u32, NativeWindow)>>::new();
        for (id, payload) in latest {
            let mut window = decode_window(&payload)?;
            if let Some(selected) = keys[&id].1 {
                window.selected = selected;
            }
            groups
                .entry(
                    window
                        .group
                        .clone()
                        .unwrap_or_else(|| format!("window-{id}")),
                )
                .or_default()
                .push((id, window));
        }
        for mut windows in groups.into_values() {
            windows.sort_by_key(|(id, window)| (window.order, *id));
            let active_tab = windows
                .iter()
                .position(|(_, window)| window.selected)
                .unwrap_or(0);
            let frame = windows[active_tab].1.frame;
            let id = workspace.id();
            let tabs = windows
                .into_iter()
                .map(|(_, window)| window.state.into_tab(&mut workspace))
                .collect::<io::Result<_>>()?;
            workspace.windows.push(WindowState {
                id,
                tabs,
                active_tab,
                frame,
                quick: false,
            });
        }
        workspace.validate()?;
        Ok(workspace)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use objc2_foundation::{NSJSONReadingOptions, NSKeyedArchiver};

        fn window_record(
            id: u32,
            title: &str,
            order: isize,
            selected: bool,
            version: isize,
        ) -> Vec<u8> {
            let mut layout = super::super::tests::ghostty_json();
            layout["titleOverride"] = title.into();
            let object = NSJSONSerialization::JSONObjectWithData_options_error(
                &NSData::with_bytes(&serde_json::to_vec(&layout).unwrap()),
                NSJSONReadingOptions::empty(),
            )
            .unwrap();
            let inner = NSKeyedArchiver::initRequiringSecureCoding(NSKeyedArchiver::alloc(), true);
            unsafe { inner.encodeObject_forKey(Some(&object), &NSString::from_str("value")) };
            let outer = NSKeyedArchiver::initRequiringSecureCoding(NSKeyedArchiver::alloc(), true);
            unsafe {
                outer.encodeObject_forKey(
                    Some(&inner.encodedData()),
                    &NSString::from_str("payload"),
                );
                for (key, value) in [
                    ("NSUIID", "TerminalWindowRestoration"),
                    ("NSWindowFrame", "{{10, 20}, {800, 600}}"),
                    ("NSTabGrpID", "group"),
                ] {
                    outer.encodeObject_forKey(
                        Some(&NSString::from_str(value)),
                        &NSString::from_str(key),
                    );
                }
                outer.encodeInteger_forKey(version, &NSString::from_str("version"));
                outer.encodeInteger_forKey(order, &NSString::from_str("NSTabIdx"));
                outer.encodeBool_forKey(selected, &NSString::from_str("NSIsKey"));
            }
            let payload = outer.encodedData().to_vec();
            let mut plain = Vec::new();
            plain.extend_from_slice(&0u32.to_be_bytes());
            plain.extend_from_slice(&9u32.to_be_bytes());
            plain.extend_from_slice(b"_NSWindowrchv");
            plain.extend_from_slice(&(payload.len() as u32).to_be_bytes());
            plain.extend_from_slice(&payload);
            plain.resize(plain.len().next_multiple_of(16), 0);
            let key = [42u8; 16];
            let mut encrypted = vec![0; plain.len()];
            let mut written = 0;
            assert_eq!(
                unsafe {
                    CCCrypt(
                        0,
                        0,
                        0,
                        key.as_ptr().cast(),
                        key.len(),
                        std::ptr::null(),
                        plain.as_ptr().cast(),
                        plain.len(),
                        encrypted.as_mut_ptr().cast(),
                        encrypted.len(),
                        &mut written,
                    )
                },
                0
            );
            assert_eq!(written, encrypted.len());
            let mut record = b"NSCR1000".to_vec();
            record.extend_from_slice(&id.to_be_bytes());
            record.extend_from_slice(&((encrypted.len() + 16) as u32).to_be_bytes());
            record.extend_from_slice(&encrypted);
            record
        }

        #[test]
        fn encrypted_log_uses_latest_window_record_and_restores_tab_order() {
            use base64::Engine;
            let dir = super::super::tests::TestDir::new();
            let key = base64::engine::general_purpose::STANDARD.encode([42u8; 16]);
            let windows = format!("<?xml version=\"1.0\"?><plist version=\"1.0\"><array>{}<dict><key>NSWindowZOrder</key><array><integer>101</integer></array></dict></array></plist>",
                [1, 2].into_iter().map(|id| format!("<dict><key>NSWindowID</key><integer>{id}</integer><key>NSWindowNumber</key><integer>{}</integer><key>NSUIID</key><string>TerminalWindowRestoration</string><key>NSDataKey</key><data>{key}</data></dict>", 100 + id)).collect::<String>());
            fs::write(dir.0.join("windows.plist"), windows.as_bytes()).unwrap();
            // An obsolete archive from an unsupported version does not prevent
            // opening a newer valid record for that window. This background
            // group's selected tab comes from window Z-order, not NSIsKey.
            let mut records = window_record(1, "obsolete", 0, false, 4);
            records.extend(window_record(2, "first", 0, false, 7));
            records.extend(window_record(1, "latest", 1, false, 7));
            fs::write(dir.0.join("data.data"), &records).unwrap();
            let state = load_layout(&dir.0).unwrap();
            assert_eq!(state.windows.len(), 1);
            assert_eq!(state.windows[0].active_tab, 1);
            assert_eq!(
                state.windows[0]
                    .tabs
                    .iter()
                    .map(|tab| tab.title.as_deref())
                    .collect::<Vec<_>>(),
                [Some("first"), Some("latest")]
            );
            assert_eq!(state.windows[0].tabs[0].panes.len(), 3);
            assert_eq!(fs::read(dir.0.join("data.data")).unwrap(), records);
            assert_eq!(
                fs::read(dir.0.join("windows.plist")).unwrap(),
                windows.as_bytes()
            );
            assert_eq!(
                load_layout(&dir.0.join("windows.plist")).unwrap().windows[0]
                    .tabs
                    .len(),
                2
            );
            records.pop();
            fs::write(dir.0.join("data.data"), records).unwrap();
            assert!(load_layout(&dir.0).is_err());
        }

        #[test]
        fn shared_archive_containers_and_deep_graphs_are_bounded_before_json() {
            let mut shared: Retained<AnyObject> = NSString::from_str("x").into();
            for _ in 0..30 {
                shared = NSArray::from_slice(&[&*shared, &*shared]).into();
            }
            assert!(bound_json(&shared).is_err());
            let mut deep: Retained<AnyObject> = NSString::from_str("x").into();
            for _ in 0..65 {
                deep = NSArray::from_slice(&[&*deep]).into();
            }
            assert!(bound_json(&deep).is_err());
            let shared = NSArray::from_slice(&[&*NSString::from_str("x")]);
            let ordinary = NSArray::from_slice(&[&*shared, &*shared]);
            bound_json(&ordinary).unwrap();
        }

        #[test]
        fn discovery_follows_the_talagent_application_mapping() {
            let dir = super::super::tests::TestDir::new();
            let container = dir.0.join("Library/Daemon Containers/container");
            let root = container.join("Data/Library/Saved Application State");
            let uuid = "00112233-4455-6677-8899-AABBCCDDEEFF";
            let source = root.join(format!("{uuid}.savedState"));
            fs::create_dir_all(&source).unwrap();
            fs::write(container.join(".com.apple.containermanagerd.metadata.plist"),
                b"<plist version=\"1.0\"><dict><key>MCMMetadataIdentifier</key><string>com.apple.talagent</string></dict></plist>").unwrap();
            fs::write(root.join("ApplicationMapping.plist"), format!(
                "<plist version=\"1.0\"><array><dict><key>protected</key><dict><key>signingIdentifier</key><string>com.mitchellh.ghostty.local</string></dict></dict><string>{uuid}</string></array></plist>"
            )).unwrap();
            fs::write(source.join("windows.plist"), b"fixture").unwrap();
            fs::write(source.join("data.data"), b"fixture").unwrap();
            let layouts = saved_layouts(&dir.0, &dir.0.join("rustty.json"));
            assert_eq!(layouts[0].path, source);
            assert!(layouts[0].available);
            assert!(!layouts[1].available);
        }

        #[test]
        #[ignore = "read-only import of RUSTTY_TEST_LAYOUT_PATH, without starting sessions"]
        fn import_saved_layout_from_environment() {
            let path =
                std::env::var_os("RUSTTY_TEST_LAYOUT_PATH").expect("set RUSTTY_TEST_LAYOUT_PATH");
            let state = load_layout(Path::new(&path)).unwrap();
            let tabs: usize = state.windows.iter().map(|window| window.tabs.len()).sum();
            let panes: usize = state
                .windows
                .iter()
                .flat_map(|window| &window.tabs)
                .map(|tab| tab.panes.len())
                .sum();
            let pane_state = state
                .windows
                .iter()
                .flat_map(|window| &window.tabs)
                .flat_map(|tab| tab.panes.values())
                .collect::<Vec<_>>();
            let titled = pane_state
                .iter()
                .filter(|pane| pane.title.is_some())
                .count();
            let overridden = pane_state
                .iter()
                .filter(|pane| pane.title_override.is_some())
                .count();
            println!(
                "Imported {} windows, {tabs} tabs, {panes} panes; {titled} saved titles, {overridden} title overrides",
                state.windows.len()
            );
            let mut live = state.clone();
            assert_eq!(
                live.append_layout(state).unwrap().len() * 2,
                live.windows.len()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustty::config::Direction;
    use std::sync::atomic::{AtomicU64, Ordering};

    pub(super) struct TestDir(pub(super) PathBuf);
    impl TestDir {
        pub(super) fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "rustty-layout-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    pub(super) fn ghostty_json() -> serde_json::Value {
        serde_json::json!({
            "focusedSurface": "second", "titleOverride": "project", "tabColor": 7,
            "surfaceTree": {
                "version": 1,
                "root": {"split": {"direction": {"horizontal": {}}, "ratio": 0.3,
                    "left": {"view": {"uuid": "first", "pwd": "/tmp/one", "title": "shell"}},
                    "right": {"split": {"direction": {"vertical": {}}, "ratio": 0.6,
                        "left": {"view": {"uuid": "second", "pwd": "/tmp/two", "title": "editor", "isUserSetTitle": true}},
                        "right": {"view": {"uuid": "third", "pwd": null}}
                    }}
                }},
                "zoomed": {"path": [{"right": {}}, {"left": {}}]},
                "quadrantZoomed": {"path": [{"right": {}}]}
            }
        })
    }

    fn workspace() -> Workspace {
        let mut workspace = Workspace::default();
        let id = workspace.id();
        let state: GhosttyState = serde_json::from_value(ghostty_json()).unwrap();
        let tab = state.into_tab(&mut workspace).unwrap();
        workspace.windows.push(WindowState {
            id,
            tabs: vec![tab],
            active_tab: 0,
            frame: [10.0, 20.0, 800.0, 600.0],
            quick: false,
        });
        workspace.validate().unwrap();
        workspace
    }

    #[test]
    fn ghostty_tree_preserves_ratios_directories_focus_and_zoom() {
        let workspace = workspace();
        let tab = &workspace.windows[0].tabs[0];
        let panes = tab.root.panes();
        assert_eq!(panes.len(), 3);
        assert_eq!(tab.focused, panes[1]);
        assert_eq!(tab.zoom, Some(panes[1]));
        assert_eq!(
            tab.panes[&panes[1]].working_directory,
            Path::new("/tmp/two")
        );
        assert!(
            tab.panes[&panes[2]]
                .working_directory
                .as_os_str()
                .is_empty()
        );
        assert_eq!(tab.panes[&panes[0]].title.as_deref(), Some("shell"));
        assert_eq!(tab.panes[&panes[0]].title_override, None);
        assert_eq!(tab.panes[&panes[1]].title.as_deref(), Some("editor"));
        assert_eq!(
            tab.panes[&panes[1]].title_override.as_deref(),
            Some("editor")
        );
        let Node::Split {
            axis,
            ratio,
            second,
            ..
        } = &tab.root.kind
        else {
            panic!()
        };
        assert_eq!(*axis, Axis::Horizontal);
        assert_eq!(*ratio, 0.3);
        assert_eq!(tab.quadrant_zoom, Some(second.id));
        assert_eq!(tab.color, Some([52, 199, 89]));
    }

    #[test]
    fn opening_layout_remaps_every_reference_and_keeps_existing_sessions() {
        let mut live = workspace();
        let tab = &mut live.windows[0].tabs[0];
        tab.remembered.insert(tab.root.id, tab.focused);
        let old = serde_json::to_value(&live.windows).unwrap();
        let saved = live.clone();
        let new_windows = live.append_layout(saved).unwrap();
        assert_eq!(new_windows, [live.windows[1].id]);
        assert_eq!(serde_json::to_value(&live.windows[..1]).unwrap(), old);
        let original = &live.windows[0].tabs[0];
        let imported = &live.windows[1].tabs[0];
        assert_ne!(original.focused, imported.focused);
        assert_eq!(imported.zoom, Some(imported.focused));
        assert_eq!(imported.remembered[&imported.root.id], imported.focused);
        assert_eq!(
            imported.panes[&imported.focused].working_directory,
            original.panes[&original.focused].working_directory
        );
        let focused = imported.focused;
        assert!(live.id() > focused);
    }

    #[test]
    fn failed_import_is_atomic_and_saved_sources_are_read_only() {
        let dir = TestDir::new();
        let source = dir.0.join("layout.json");
        let mut live = workspace();
        live.save(&source).unwrap();
        let bytes = fs::read(&source).unwrap();
        let loaded = load_layout(&source).unwrap();
        live.append_layout(loaded).unwrap();
        assert_eq!(fs::read(&source).unwrap(), bytes);
        let before = serde_json::to_value(&live).unwrap();
        let mut broken = live.clone();
        broken.windows[0].tabs[0].focused = u64::MAX;
        assert!(live.append_layout(broken).is_err());
        assert_eq!(serde_json::to_value(&live).unwrap(), before);
        let pane = live.id();
        let split = live.id();
        live.windows[0].tabs[0].split(pane, split, Direction::Down, PathBuf::from("/tmp"));
        live.validate().unwrap();
    }

    #[test]
    fn discovery_lists_defaults_without_creating_files() {
        let dir = TestDir::new();
        let state = dir.0.join("rustty.json");
        let defaults = saved_layouts(&dir.0, &state);
        assert_eq!(
            defaults
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["Ghostty Local", "Ghostty", "Rustty"]
        );
        assert!(defaults.iter().all(|entry| !entry.available));
        assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 0);
        workspace().save(&state).unwrap();
        assert!(saved_layouts(&dir.0, &state)[2].available);
    }

    #[test]
    fn existing_saved_panes_without_title_metadata_still_load() {
        let pane: SavedPane = serde_json::from_str(r#"{"working_directory":"/tmp"}"#).unwrap();
        assert_eq!(pane.working_directory, Path::new("/tmp"));
        assert_eq!(pane.title, None);
        assert_eq!(pane.title_override, None);
    }
}
