use super::*;

impl Layout {
    #[allow(clippy::too_many_arguments)]
    pub fn split_floating(
        &mut self,
        target: u32,
        id: u32,
        axis: Axis,
        before: bool,
        full: bool,
        border: bool,
        viewport: (u32, u32),
    ) -> Result<()> {
        let cell = self.find(target).ok_or_else(|| invalid("pane not found"))?;
        if !cell.is_floating() {
            return Err(invalid("pane is not floating"));
        }
        let g = cell.geometry;
        let (left, right, top, bottom, pad) = (
            3_i64,
            i64::from(viewport.0) - 3,
            1_i64,
            i64::from(viewport.1) - 1,
            i64::from(border),
        );
        let (mut x, mut y, mut w, mut h) = (
            i64::from(g.x),
            i64::from(g.y),
            i64::from(g.width),
            i64::from(g.height),
        );
        x = x.max(left + pad).min(right - w - pad);
        y = y.max(top + pad).min(bottom - h - pad);
        let (mut nx, mut ny, mut nw, mut nh) = (x, y, w, h);
        let sign = if before { -1 } else { 1 };
        if axis == Axis::Horizontal {
            nx += sign * (w + 2 * pad);
        } else {
            ny += sign * (h + 2 * pad);
        }
        if left > nx - pad {
            let space = x + w - left - 3 * pad + 1;
            nw = space / 2;
            w = nw;
            nx = left + pad;
            x = nx + nw + 2 * pad;
            if space % 2 == 0 {
                w -= 1;
            }
        } else if right < nx + nw + pad {
            let space = right - x - 3 * pad + 1;
            nw = space / 2;
            w = nw;
            nx = x + w + 2 * pad;
            if space % 2 == 0 {
                nw -= 1;
            }
        } else if top > ny - pad {
            let space = h + y - top - 3 * pad + 1;
            nh = space / 2;
            h = nh;
            ny = top + pad;
            y = ny + nh + 2 * pad;
            if space % 2 == 0 {
                h -= 1;
            }
        } else if bottom < ny + nh + pad {
            let space = bottom - y - 3 * pad + 1;
            nh = space / 2;
            h = nh;
            ny = y + h + 2 * pad;
            if space % 2 == 0 {
                nh -= 1;
            }
        }
        if full {
            if axis == Axis::Horizontal {
                ny = top + pad;
                nh = bottom - top - 2 * pad;
                if before {
                    nx = left + pad;
                    nw = x - nx - 2 * pad;
                } else {
                    nw = right - nx - pad;
                }
            } else {
                nx = left + pad;
                nw = right - left - 2 * pad;
                if before {
                    ny = top + pad;
                    nh = y - ny - 2 * pad;
                } else {
                    nh = bottom - ny - pad;
                }
            }
        }
        if [w, h, nw, nh]
            .iter()
            .any(|n| !(1..=i64::from(MAX_DIMENSION)).contains(n))
        {
            return Err(invalid("no space for a new pane"));
        }
        let mut layout = self.clone();
        layout.find_mut(target).unwrap().geometry = Geometry {
            x: x as i32,
            y: y as i32,
            width: w as u32,
            height: h as u32,
        };
        layout.insert_floating(
            target,
            id,
            Geometry {
                x: nx as i32,
                y: ny as i32,
                width: nw as u32,
                height: nh as u32,
            },
        )?;
        *self = layout;
        Ok(())
    }
    pub fn set_floating_z(&mut self, id: u32, z: usize) -> Result<()> {
        let mut order: Vec<_> = self.panes().iter().filter_map(|p| p.z.zip(p.id)).collect();
        order.sort_unstable();
        let at = order
            .iter()
            .position(|(_, pane)| *pane == id)
            .ok_or_else(|| invalid("pane is not floating"))?;
        let pane = order.remove(at);
        order.insert(z.min(order.len()), pane);
        for (z, (_, id)) in order.into_iter().enumerate() {
            if let Kind::Pane(pane) = &mut self.find_mut(id).unwrap().kind {
                pane.z = Some(z as u32);
            }
        }
        Ok(())
    }
    pub fn insert_floating(&mut self, target: u32, id: u32, geometry: Geometry) -> Result<()> {
        self.validate_tiled()?;
        check_geometry(geometry)?;
        if self.find(id).is_some() {
            return Err(invalid("pane ID already exists"));
        }
        if self.find(target).is_none() {
            return Err(invalid("pane not found"));
        }
        let new = Self {
            geometry,
            kind: Kind::Pane(Pane {
                id: Some(id),
                z: Some(0),
                ..Pane::default()
            }),
        };
        fn insert(layout: &mut Layout, target: u32, new: &Layout) -> bool {
            match &mut layout.kind {
                Kind::Pane(_) => false,
                Kind::Split { children, .. } => {
                    if let Some(index) = children
                        .iter()
                        .position(|c| matches!(&c.kind, Kind::Pane(p) if p.id == Some(target)))
                    {
                        children.insert(index + 1, new.clone());
                        true
                    } else {
                        children.iter_mut().any(|c| insert(c, target, new))
                    }
                }
            }
        }
        let mut layout = self.clone();
        let existing: Vec<_> = layout
            .panes()
            .iter()
            .filter_map(|p| p.z.zip(p.id))
            .collect();
        for (z, id) in existing {
            if let Kind::Pane(p) = &mut layout.find_mut(id).unwrap().kind {
                p.z = Some(z + 1);
            }
        }
        if matches!(layout.kind, Kind::Pane(_)) {
            let old = layout.clone();
            layout.kind = Kind::Split {
                axis: Axis::Vertical,
                children: vec![old, new],
            };
        } else {
            insert(&mut layout, target, &new);
        }
        layout.validate_tiled()?;
        *self = layout;
        Ok(())
    }
    pub fn is_floating(&self) -> bool {
        matches!(&self.kind, Kind::Pane(pane) if pane.z.is_some())
    }
    pub fn has_tiled(&self) -> bool {
        match &self.kind {
            Kind::Pane(pane) => pane.z.is_none(),
            Kind::Split { children, .. } => children.iter().any(Self::has_tiled),
        }
    }
    pub fn normalize_z(&mut self) {
        let mut order: Vec<_> = self.panes().iter().filter_map(|p| p.z.zip(p.id)).collect();
        order.sort_unstable();
        for (z, (_, id)) in order.into_iter().enumerate() {
            if let Kind::Pane(pane) = &mut self.find_mut(id).unwrap().kind {
                pane.z = Some(z as u32);
            }
        }
    }
    pub fn append_floating(&mut self, panes: Vec<Layout>, in_other: bool) -> Result<()> {
        if panes.is_empty() {
            return Ok(());
        }
        if panes.iter().any(|p| !p.is_floating()) {
            return Err(invalid("expected floating panes"));
        }
        let mut layout = self.clone();
        let parent = if in_other {
            match &mut layout.kind {
                Kind::Split { children, .. } => children
                    .iter_mut()
                    .find(|p| matches!(p.kind, Kind::Split { .. }))
                    .ok_or_else(|| invalid("missing other-pane split"))?,
                _ => return Err(invalid("missing main-pane split")),
            }
        } else {
            &mut layout
        };
        match &mut parent.kind {
            Kind::Split { children, .. } => children.extend(panes),
            Kind::Pane(_) => {
                let mut children = vec![parent.clone()];
                children.extend(panes);
                parent.kind = Kind::Split {
                    axis: Axis::Vertical,
                    children,
                };
            }
        }
        layout.validate_tiled()?;
        *self = layout;
        Ok(())
    }
    pub fn tiled_copy(&self) -> Option<Self> {
        match &self.kind {
            Kind::Pane(pane) => pane.z.is_none().then(|| self.clone()),
            Kind::Split { axis, children } => {
                let mut children: Vec<_> = children.iter().filter_map(Self::tiled_copy).collect();
                match children.len() {
                    0 => None,
                    1 => children.pop(),
                    _ => Some(Self {
                        geometry: self.geometry,
                        kind: Kind::Split {
                            axis: *axis,
                            children,
                        },
                    }),
                }
            }
        }
    }

    pub fn find_mut(&mut self, id: u32) -> Option<&mut Self> {
        if matches!(&self.kind, Kind::Pane(pane) if pane.id == Some(id)) {
            return Some(self);
        }
        match &mut self.kind {
            Kind::Pane(_) => None,
            Kind::Split { children, .. } => children.iter_mut().find_map(|c| c.find_mut(id)),
        }
    }

    pub fn float_pane(&mut self, id: u32, geometry: Geometry) -> Result<()> {
        self.validate_tiled()?;
        check_geometry(geometry)?;
        let pane = self.find(id).ok_or_else(|| invalid("pane not found"))?;
        if pane.is_floating() {
            return Err(invalid("pane is already floating"));
        }
        fn convert(layout: &mut Layout, id: u32, geometry: Geometry) -> bool {
            match &mut layout.kind {
                Kind::Pane(pane) => {
                    if pane.id != Some(id) {
                        return false;
                    }
                    pane.z = Some(0);
                    layout.geometry = geometry;
                }
                Kind::Split { axis, children } => {
                    let Some(at) = children.iter().position(|c| c.find(id).is_some()) else {
                        return false;
                    };
                    let old = children[at].geometry;
                    if !convert(&mut children[at], id, geometry) {
                        return false;
                    }
                    if !children[at].has_tiled() {
                        let neighbor = ((at + 1)..children.len())
                            .chain((0..at).rev())
                            .find(|index| children[*index].has_tiled());
                        if let Some(index) = neighbor {
                            let mut next = children[index].geometry;
                            match axis {
                                Axis::Horizontal => next.width += old.width + 1,
                                Axis::Vertical => next.height += old.height + 1,
                            }
                            children[index].position(next);
                        }
                        if matches!(children[at].kind, Kind::Split { .. }) {
                            children[at].geometry = Geometry {
                                width: 0,
                                height: 0,
                                x: 0,
                                y: 0,
                            };
                        }
                    }
                    layout.position(layout.geometry);
                }
            }
            true
        }
        let mut layout = self.clone();
        for pane_id in layout
            .panes()
            .iter()
            .filter(|p| p.z.is_some())
            .filter_map(|p| p.id)
            .collect::<Vec<_>>()
        {
            if let Kind::Pane(pane) = &mut layout.find_mut(pane_id).unwrap().kind {
                pane.z = pane.z.map(|z| z + 1);
            }
        }
        convert(&mut layout, id, geometry);
        layout.validate_tiled()?;
        *self = layout;
        Ok(())
    }

    pub fn position_floating(&mut self, id: u32, geometry: Geometry) -> Result<()> {
        check_geometry(geometry)?;
        let pane = self.find_mut(id).ok_or_else(|| invalid("pane not found"))?;
        if !pane.is_floating() {
            return Err(invalid("pane is not floating"));
        }
        pane.geometry = geometry;
        Ok(())
    }

    pub fn tile_pane(&mut self, id: u32, geometry: Geometry) -> Result<()> {
        if !self
            .find(id)
            .ok_or_else(|| invalid("pane not found"))?
            .is_floating()
        {
            return Err(invalid("pane is not floating"));
        }
        fn tile(layout: &mut Layout, id: u32, geometry: Geometry) -> Result<()> {
            let Kind::Split { axis, children } = &mut layout.kind else {
                let Kind::Pane(pane) = &mut layout.kind else {
                    unreachable!()
                };
                pane.z = None;
                layout.geometry = geometry;
                return Ok(());
            };
            let at = children
                .iter()
                .position(|c| c.find(id).is_some())
                .ok_or_else(|| invalid("pane not found"))?;
            let child_geometry = if children[at].has_tiled() {
                children[at].geometry
            } else if let Some(index) = ((at + 1)..children.len())
                .chain((0..at).rev())
                .find(|index| children[*index].has_tiled())
            {
                let old = children[index].geometry;
                let size = if *axis == Axis::Horizontal {
                    old.width
                } else {
                    old.height
                };
                if size < 3 {
                    return Err(invalid("no space for a new pane"));
                }
                let first = size / 2;
                let second = size - first - 1;
                let mut neighbor = old;
                let mut new = old;
                match axis {
                    Axis::Horizontal => {
                        neighbor.width = second;
                        new.width = first;
                    }
                    Axis::Vertical => {
                        neighbor.height = second;
                        new.height = first;
                    }
                }
                let min = children[index].minimum();
                if neighbor.width < min.0 || neighbor.height < min.1 {
                    return Err(invalid("no space for a new pane"));
                }
                children[index].position(neighbor);
                new
            } else {
                geometry
            };
            tile(&mut children[at], id, child_geometry)?;
            layout.position(geometry);
            Ok(())
        }
        let mut layout = self.clone();
        tile(&mut layout, id, geometry)?;
        layout.normalize_z();
        layout.validate_tiled()?;
        *self = layout;
        Ok(())
    }
}
