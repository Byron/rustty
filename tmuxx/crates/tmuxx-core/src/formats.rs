use super::*;

impl Engine {
    pub(super) fn expand_depth(
        &self,
        format: &[u8],
        target: (Option<Id>, Option<Id>, Option<Id>),
        context: &ClientContext,
        depth: usize,
        time: bool,
    ) -> Result<Vec<u8>, String> {
        let (sid, wid, pid) = target;
        // shortcut: context recursion is capped below C until evaluation uses an explicit stack.
        if depth > 16 {
            return Err("format context nesting limit exceeded".into());
        }
        let typed = context.format_type.is_none().then(|| {
            let mut context = context.clone();
            context.format_type = Some(if pid.is_some() {
                b'P'
            } else if wid.is_some() {
                b'W'
            } else if sid.is_some() {
                b'S'
            } else {
                0
            });
            context
        });
        let context = typed.as_ref().unwrap_or(context);
        let sid = sid.or_else(|| {
            context
                .has_format_client()
                .then_some(context.session)
                .flatten()
        });
        let wid = wid.or_else(|| {
            let session = self.sessions.get(&sid?)?;
            session.windows.get(&session.active).copied()
        });
        let pid = pid.or_else(|| self.windows.get(&wid?).map(|window| window.active));
        let target = (sid, wid, pid);
        let mut failure = None;
        let lookup = |key: &[u8]| {
            if let Some(command) = key.strip_prefix(tmuxx_format::JOB_PREFIX) {
                return Some(match self.format_job(command, target, context, depth) {
                    Ok(output) => output,
                    Err(error) => {
                        failure = Some(error);
                        Vec::new()
                    }
                });
            }
            if let Some(value) = context.formats.get(key) {
                return Some(value.clone());
            }
            if key == b"command"
                && let Some(command) = context.command
            {
                return Some(command.as_bytes().to_vec());
            }
            if let Some(value) = self.buffer_format(key, None) {
                return Some(value);
            }
            if let Some(value) = self.context_format(key, (sid, wid, pid), context, depth) {
                return Some(match value {
                    Ok(value) => value,
                    Err(error) => {
                        failure = Some(error);
                        Vec::new()
                    }
                });
            }
            // OSC 7 is a byte payload. Keep it exact when a format is reused as
            // a cwd; the native path admission boundary can then reject it.
            if key == b"pane_path" {
                return self
                    .panes
                    .get(&pid?)
                    .map(|pane| pane.terminal.cwd().to_vec());
            }
            let key = std::str::from_utf8(key).ok()?;
            self.format_value(key, sid, wid, pid, context)
                .map(String::into_bytes)
        };
        let result = if time {
            tmuxx_format::expand_time(format, lookup)
        } else {
            tmuxx_format::expand(format, lookup)
        }
        .map_err(|error| error.to_string())?;
        if let Some(error) = failure {
            return Err(error);
        }
        if result.len() > 1024 * 1024 {
            return Err("format output exceeds limit".into());
        }
        Ok(result)
    }

    fn context_format(
        &self,
        key: &[u8],
        target: (Option<Id>, Option<Id>, Option<Id>),
        context: &ClientContext,
        depth: usize,
    ) -> Option<Result<Vec<u8>, String>> {
        let (sid, wid, _) = target;
        let separator = key.iter().position(|b| *b == b':')?;
        let (modifier, body) = (&key[..separator], &key[separator + 1..]);
        let kind = *modifier.first()?;
        if kind == b'C' {
            return Some((|| {
                let needle = self.expand_depth(body, target, context, depth + 1, false)?;
                let Some(pane) = target.2.and_then(|id| self.panes.get(&id)) else {
                    return Ok(b"0".to_vec());
                };
                let flags = modifier.get(2..).unwrap_or_default();
                let screen = pane.terminal.snapshot();
                let screen = if screen.alternate_active {
                    screen.alternate.as_ref().unwrap_or(&screen.primary)
                } else {
                    &screen.primary
                };
                let mut found = None;
                for row in 0..screen.height() {
                    let text = screen.row_text(screen.row(row));
                    let text = text.trim_end_matches(|c: char| c.is_ascii_whitespace());
                    if tmuxx_format::content_match(
                        &needle,
                        text.as_bytes(),
                        flags.contains(&b'r'),
                        flags.contains(&b'i'),
                    )
                    .map_err(|error| error.to_string())?
                    {
                        found = Some(row);
                        break;
                    }
                }
                Ok(found.map_or(0, |row| row + 1).to_string().into_bytes())
            })());
        }
        if matches!(kind, b'O' | b'V' | b'L') {
            return Some(self.collection_format(
                kind,
                modifier.get(2..).unwrap_or_default(),
                body,
                target,
                context,
                depth,
            ));
        }
        if !matches!(kind, b'S' | b'W' | b'P' | b'N') {
            return None;
        }
        Some((|| {
            let flags = modifier.get(2..).unwrap_or_default();
            if kind == b'N' {
                let name = self.expand_depth(body, (sid, wid, None), context, depth + 1, false)?;
                let found = if flags.contains(&b's') && !flags.contains(&b'w') {
                    self.sessions
                        .values()
                        .any(|session| session.name.as_bytes() == name)
                } else {
                    sid.and_then(|sid| self.sessions.get(&sid))
                        .is_some_and(|session| {
                            session
                                .windows
                                .values()
                                .any(|wid| self.windows[wid].name.as_bytes() == name)
                        })
                };
                return Ok(if found { b"1".to_vec() } else { b"0".to_vec() });
            }
            let mut targets = Vec::new();
            match kind {
                b'S' => {
                    let mut sessions = self.sessions.values().collect::<Vec<_>>();
                    sessions.sort_by(|a, b| {
                        let order = if flags.contains(&b'i') {
                            a.id.cmp(&b.id)
                        } else if flags.contains(&b'n') {
                            a.name.cmp(&b.name)
                        } else if flags.contains(&b't') {
                            b.activity.cmp(&a.activity).then(a.name.cmp(&b.name))
                        } else {
                            a.id.cmp(&b.id)
                        };
                        if flags.contains(&b'r') {
                            order.reverse()
                        } else {
                            order
                        }
                    });
                    for session in sessions {
                        let window = session.windows.get(&session.active).copied();
                        let pane = window
                            .and_then(|wid| self.windows.get(&wid))
                            .map(|window| window.active);
                        targets.push((
                            Some(session.id),
                            window,
                            pane,
                            Some(session.active),
                            context.control && context.session == Some(session.id),
                        ));
                    }
                }
                b'W' => {
                    if let Some(session) = sid.and_then(|sid| self.sessions.get(&sid)) {
                        let mut windows = session.windows.iter().collect::<Vec<_>>();
                        if !flags.contains(&b'i')
                            && (flags.contains(&b'n') || flags.contains(&b't'))
                        {
                            windows.sort_by(|a, b| {
                                let (wa, wb) = (&self.windows[a.1], &self.windows[b.1]);
                                let order = if flags.contains(&b'n') {
                                    wa.name.cmp(&wb.name)
                                } else {
                                    wb.activity.cmp(&wa.activity).then(wa.name.cmp(&wb.name))
                                };
                                if flags.contains(&b'r') {
                                    order.reverse()
                                } else {
                                    order
                                }
                            });
                        } else if flags.contains(&b'r') {
                            windows.reverse();
                        }
                        for (index, id) in windows {
                            targets.push((
                                sid,
                                Some(*id),
                                Some(self.windows[id].active),
                                Some(*index),
                                *index == session.active,
                            ));
                        }
                    }
                }
                b'P' => {
                    if let Some(window) = wid.and_then(|wid| self.windows.get(&wid)) {
                        let mut panes = window.panes.clone();
                        if !flags.contains(&b'i') {
                            if flags.contains(&b'z') {
                                panes.sort_by(|a, b| {
                                    let order =
                                        window.pane_z(*a).cmp(&window.pane_z(*b)).then_with(|| {
                                            self.panes[a]
                                                .terminal
                                                .title()
                                                .cmp(self.panes[b].terminal.title())
                                        });
                                    if flags.contains(&b'r') {
                                        order.reverse()
                                    } else {
                                        order
                                    }
                                });
                            } else {
                                panes.sort_unstable();
                            }
                        }
                        for id in panes {
                            targets.push((sid, wid, Some(id), None, id == window.active));
                        }
                    }
                }
                _ => unreachable!(),
            }
            if kind == b'P'
                && flags.contains(&b'r')
                && (!flags.contains(&b'z') || flags.contains(&b'i'))
            {
                targets.reverse();
            }
            let (all, active) = tmuxx_format::loop_branches(body);
            let mut out = Vec::new();
            for (index, &(sid, wid, pid, window_index, selected)) in targets.iter().enumerate() {
                let mut context = context.clone();
                context.formats.clear();
                context.format_type = Some(kind);
                context
                    .formats
                    .insert(b"loop_index".to_vec(), index.to_string().into_bytes());
                context.formats.insert(
                    b"loop_last_flag".to_vec(),
                    u8::from(index + 1 == targets.len())
                        .to_string()
                        .into_bytes(),
                );
                if let (Some(sid), Some(window_index)) = (sid, window_index) {
                    context = self.winlink_context(&context, sid, window_index);
                }
                if kind == b'W' {
                    let is_active = |other: usize| targets.get(other).is_some_and(|t| t.4);
                    context.formats.insert(
                        b"window_after_active".to_vec(),
                        u8::from(index > 0 && is_active(index - 1))
                            .to_string()
                            .into_bytes(),
                    );
                    context.formats.insert(
                        b"window_before_active".to_vec(),
                        u8::from(is_active(index + 1)).to_string().into_bytes(),
                    );
                    for (prefix, position) in [
                        ("prev", index.checked_sub(1)),
                        ("next", index.checked_add(1)),
                    ] {
                        if let Some((_, Some(wid), _, Some(window_index), selected)) =
                            position.and_then(|p| targets.get(p))
                        {
                            context.formats.insert(
                                format!("{prefix}_window_index").into_bytes(),
                                window_index.to_string().into_bytes(),
                            );
                            context.formats.insert(
                                format!("{prefix}_window_active").into_bytes(),
                                u8::from(*selected).to_string().into_bytes(),
                            );
                            for (key, value) in &self.windows[wid].options {
                                if key.starts_with('@') {
                                    context.formats.insert(
                                        format!("{prefix}_{key}").into_bytes(),
                                        value.as_bytes().to_vec(),
                                    );
                                }
                            }
                        }
                    }
                }
                out.extend(self.expand_depth(
                    if selected { active.unwrap_or(all) } else { all },
                    (sid, wid, pid),
                    &context,
                    depth + 1,
                    false,
                )?);
                if out.len() > 1024 * 1024 {
                    return Err("format loop output exceeds limit".into());
                }
            }
            Ok(out)
        })())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_content_formats_search_visible_unicode_rows_and_alternate_screen() {
        let mut engine = Engine::default();
        let mut context = ClientContext::default();
        let command = Command {
            blocks: Default::default(),
            args: ["new-session", "-d", "-s", "content", ""]
                .into_iter()
                .map(|arg| arg.as_bytes().to_vec())
                .collect(),
        };
        engine.execute(&command, &mut context).unwrap();
        engine.feed(0, "first\r\nÉté 界面".as_bytes());
        for (format, expected) in [
            ("#{C/ri:été}", "2"),
            ("#{C:É?? ??}", "2"),
            ("#{C/r:^[[:alpha:]]+ ..$}", "2"),
            ("#{C/r:[}", "0"),
            ("#{C:missing}", "0"),
        ] {
            assert_eq!(
                engine
                    .expand(format.as_bytes(), Some(0), Some(0), Some(0), &context)
                    .unwrap(),
                expected.as_bytes(),
                "{format}"
            );
        }
        engine.feed(0, b"\x1b[?1049h\x1b[HALTERNATE");
        assert_eq!(
            engine
                .expand(b"#{C/r:^ALTERNATE$}", Some(0), Some(0), Some(0), &context)
                .unwrap(),
            b"1"
        );
        assert_eq!(
            engine
                .expand("#{C:Été}".as_bytes(), Some(0), Some(0), Some(0), &context)
                .unwrap(),
            b"0"
        );
        engine.feed(0, b"\x1b[?1049l");
        assert_eq!(
            engine
                .expand("#{C:Été}".as_bytes(), Some(0), Some(0), Some(0), &context)
                .unwrap(),
            b"2"
        );
    }

    #[cfg(windows)]
    #[test]
    fn pane_content_regex_resource_errors_are_not_reported_as_no_match() {
        let mut engine = Engine::default();
        let mut context = ClientContext::default();
        engine
            .execute(
                &Command {
                    blocks: Default::default(),
                    args: ["new-session", "-d", ""]
                        .into_iter()
                        .map(|arg| arg.as_bytes().to_vec())
                        .collect(),
                },
                &mut context,
            )
            .unwrap();
        engine.feed(0, &b"a".repeat(48));
        assert!(
            engine
                .expand(b"#{C/r:(a|aa)*b}", Some(0), Some(0), Some(0), &context)
                .unwrap_err()
                .contains("work limit")
        );
        context
            .formats
            .insert(b"needle".to_vec(), b"a".repeat(65537));
        assert!(
            engine
                .expand(b"#{C/r:#{needle}}", Some(0), Some(0), Some(0), &context)
                .unwrap_err()
                .contains("complexity limit")
        );
    }

    #[test]
    fn context_recursion_and_loop_output_are_bounded() {
        let mut engine = Engine::default();
        let mut context = ClientContext::default();
        let mut run = |args: &[&str]| {
            let command = Command {
                blocks: Default::default(),
                args: args.iter().map(|arg| arg.as_bytes().to_vec()).collect(),
            };
            engine.execute(&command, &mut context).unwrap();
        };
        for index in 0..17 {
            run(&[
                "new-session",
                "-d",
                "-s",
                &format!("bounded{index}"),
                "sleep 30",
            ]);
        }
        run(&["set-option", "-g", "@recursive", "#{S:#{E:@recursive}}"]);
        assert!(
            engine
                .expand(b"#{E:@recursive}", None, None, None, &context)
                .unwrap_err()
                .contains("nesting limit")
        );
        assert!(
            engine
                .expand(b"#{S:#{R:abcdefgh,8192}}", None, None, None, &context)
                .unwrap_err()
                .contains("output exceeds limit")
        );
    }
}
