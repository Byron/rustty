use super::*;

pub const PRESETS: &[&str] = &[
    "even-horizontal",
    "even-vertical",
    "main-horizontal",
    "main-horizontal-mirrored",
    "main-vertical",
    "main-vertical-mirrored",
    "tiled",
];

fn node(geometry: Geometry, axis: Axis, children: Vec<Layout>) -> Layout {
    if children.len() == 1 {
        return children.into_iter().next().unwrap();
    }
    Layout {
        geometry,
        kind: Kind::Split { axis, children },
    }
}
fn leaf(id: u32, geometry: Geometry) -> Layout {
    Layout {
        geometry,
        kind: Kind::Pane(Pane {
            id: Some(id),
            ..Pane::default()
        }),
    }
}
fn even(ids: &[u32], geometry: Geometry, axis: Axis) -> Layout {
    let size = if axis == Axis::Horizontal {
        geometry.width
    } else {
        geometry.height
    };
    let count = ids.len() as u32;
    let available = size - count + 1;
    let mut offset = 0;
    let children = ids
        .iter()
        .enumerate()
        .map(|(index, id)| {
            let size = available / count + u32::from((index as u32) < available % count);
            let geometry = match axis {
                Axis::Horizontal => Geometry {
                    x: geometry.x + offset,
                    width: size,
                    ..geometry
                },
                Axis::Vertical => Geometry {
                    y: geometry.y + offset,
                    height: size,
                    ..geometry
                },
            };
            offset += size as i32 + 1;
            leaf(*id, geometry)
        })
        .collect();
    node(geometry, axis, children)
}

impl Layout {
    pub fn preset(
        ids: &[u32],
        mut geometry: Geometry,
        preset: usize,
        main: u32,
        other: u32,
        max_columns: u32,
    ) -> Result<Self> {
        if ids.is_empty() {
            return Err(invalid("layout needs at least one pane"));
        }
        let count = u32::try_from(ids.len()).map_err(|_| invalid("too many panes"))?;
        if count > MAX_DIMENSION {
            return Err(invalid("too many panes"));
        }
        if count == 1 {
            return Ok(leaf(ids[0], geometry));
        }
        let layout = match preset {
            0 | 1 => {
                let axis = if preset == 0 {
                    Axis::Horizontal
                } else {
                    Axis::Vertical
                };
                if preset == 0 {
                    geometry.width = geometry.width.max(2 * count - 1);
                } else {
                    geometry.height = geometry.height.max(2 * count - 1);
                }
                even(ids, geometry, axis)
            }
            2..=5 => {
                let vertical = preset >= 4;
                let mirrored = preset % 2 == 1;
                let available = if vertical {
                    geometry.width
                } else {
                    geometry.height
                }
                .saturating_sub(1);
                let (main, other) = if main.saturating_add(1) >= available {
                    (available.saturating_sub(1).max(1), 1)
                } else if other == 0 || other > available - main {
                    (main, available - main)
                } else {
                    (available - other, other)
                };
                let (main_geometry, other_geometry) = if vertical {
                    geometry.height = geometry.height.max(2 * (count - 1) - 1);
                    geometry.width = main + other + 1;
                    (
                        Geometry {
                            width: main,
                            x: geometry.x + if mirrored { other as i32 + 1 } else { 0 },
                            ..geometry
                        },
                        Geometry {
                            width: other,
                            x: geometry.x + if mirrored { 0 } else { main as i32 + 1 },
                            ..geometry
                        },
                    )
                } else {
                    geometry.width = geometry.width.max(2 * (count - 1) - 1);
                    geometry.height = main + other + 1;
                    (
                        Geometry {
                            height: main,
                            y: geometry.y + if mirrored { other as i32 + 1 } else { 0 },
                            ..geometry
                        },
                        Geometry {
                            height: other,
                            y: geometry.y + if mirrored { 0 } else { main as i32 + 1 },
                            ..geometry
                        },
                    )
                };
                let main = leaf(ids[0], main_geometry);
                let other = even(
                    &ids[1..],
                    other_geometry,
                    if vertical {
                        Axis::Vertical
                    } else {
                        Axis::Horizontal
                    },
                );
                node(
                    geometry,
                    if vertical {
                        Axis::Horizontal
                    } else {
                        Axis::Vertical
                    },
                    if mirrored {
                        vec![other, main]
                    } else {
                        vec![main, other]
                    },
                )
            }
            6 => {
                let (mut rows, mut columns) = (1, 1);
                while rows * columns < count {
                    rows += 1;
                    if rows * columns < count && (max_columns == 0 || columns < max_columns) {
                        columns += 1;
                    }
                }
                let width = (geometry.width.saturating_sub(columns - 1) / columns).max(1);
                let height = (geometry.height.saturating_sub(rows - 1) / rows).max(1);
                geometry.width = geometry.width.max((width + 1) * columns - 1);
                geometry.height = geometry.height.max((height + 1) * rows - 1);
                let children = ids
                    .chunks(columns as usize)
                    .enumerate()
                    .map(|(row, ids)| {
                        let y = geometry.y + row as i32 * (height + 1) as i32;
                        let row_height = if row + 1 == rows as usize {
                            geometry.height - row as u32 * (height + 1)
                        } else {
                            height
                        };
                        let row_geometry = Geometry {
                            height: row_height,
                            y,
                            ..geometry
                        };
                        let children = ids
                            .iter()
                            .enumerate()
                            .map(|(column, id)| {
                                let x = geometry.x + column as i32 * (width + 1) as i32;
                                let width = if column + 1 == ids.len() {
                                    geometry.width - column as u32 * (width + 1)
                                } else {
                                    width
                                };
                                leaf(
                                    *id,
                                    Geometry {
                                        width,
                                        x,
                                        ..row_geometry
                                    },
                                )
                            })
                            .collect();
                        node(row_geometry, Axis::Horizontal, children)
                    })
                    .collect();
                node(geometry, Axis::Vertical, children)
            }
            _ => return Err(invalid("unknown layout preset")),
        };
        layout.validate_tiled()?;
        Ok(layout)
    }

    /// Spread the nearest ancestor that has unequal children, as select-layout -E.
    pub fn spread(&mut self, id: u32) -> Result<()> {
        self.validate_tiled()?;
        fn spread(layout: &mut Layout, id: u32) -> bool {
            let geometry = layout.geometry;
            let Kind::Split { axis, children } = &mut layout.kind else {
                return false;
            };
            if let Some(child) = children.iter_mut().find(|c| c.find(id).is_some()) {
                if spread(child, id) {
                    return true;
                }
            } else {
                return false;
            }
            let mut children: Vec<_> = children.iter_mut().filter(|c| c.has_tiled()).collect();
            if children.len() < 2 {
                return false;
            }
            let size = if *axis == Axis::Horizontal {
                geometry.width
            } else {
                geometry.height
            };
            let available = size - children.len() as u32 + 1;
            let count = children.len() as u32;
            let mut changed = false;
            for (index, child) in children.iter_mut().enumerate() {
                let length = available / count + u32::from((index as u32) < available % count);
                let min = child.minimum();
                let length = length.max(if *axis == Axis::Horizontal {
                    min.0
                } else {
                    min.1
                });
                let next = if *axis == Axis::Horizontal {
                    Geometry {
                        width: length,
                        ..child.geometry
                    }
                } else {
                    Geometry {
                        height: length,
                        ..child.geometry
                    }
                };
                changed |= next != child.geometry;
                child.position(next);
            }
            layout.position(geometry);
            changed
        }
        let mut result = self.clone();
        spread(&mut result, id);
        result.validate_tiled()?;
        *self = result;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn presets_fit_panes_even_at_minimum_sizes_and_keep_identity() {
        for count in 1..=16 {
            let ids: Vec<_> = (0..count).map(|id| id * 7).collect();
            for width in [1, 7, 80] {
                for height in [1, 6, 24] {
                    for preset in 0..PRESETS.len() {
                        for max_columns in [0, 1, 3] {
                            let geometry = Geometry {
                                width,
                                height,
                                x: 0,
                                y: 0,
                            };
                            let layout =
                                Layout::preset(&ids, geometry, preset, 20, 0, max_columns).unwrap();
                            layout.validate_tiled().unwrap();
                            let mut actual: Vec<_> =
                                layout.panes().iter().map(|p| p.id.unwrap()).collect();
                            actual.sort_unstable();
                            assert_eq!(actual, ids);
                        }
                    }
                }
            }
        }
    }
}
