//! Prepare tmux configuration against supplied state, then return its command queue.
//!
//! Assignments and conditionals take effect during preparation. Commands such as
//! `set-environment`, `set-option`, and `source-file` remain queued commands; this
//! crate does not execute them or access the filesystem or process environment.

use std::collections::{BTreeMap, BTreeSet};
pub use tmuxx_command::Error;
use tmuxx_command::{
    Command, Reader, Token, TokenKind, serialize, variable_continue, variable_start,
};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Environment {
    pub values: BTreeMap<Vec<u8>, Vec<u8>>,
    pub hidden: BTreeSet<Vec<u8>>,
}

impl Environment {
    pub fn get(&self, name: &[u8]) -> Option<&[u8]> {
        self.values.get(name).map(Vec::as_slice)
    }

    pub fn set(&mut self, name: Vec<u8>, value: Vec<u8>, hidden: bool) {
        if hidden {
            self.hidden.insert(name.clone());
        } else {
            self.hidden.remove(&name);
        }
        self.values.insert(name, value);
    }
}

impl FromIterator<(Vec<u8>, Vec<u8>)> for Environment {
    fn from_iter<T: IntoIterator<Item = (Vec<u8>, Vec<u8>)>>(iter: T) -> Self {
        Self {
            values: iter.into_iter().collect(),
            hidden: BTreeSet::new(),
        }
    }
}

/// Evaluate preparation-time syntax using current supplied values. Runtime
/// command effects cannot affect later conditions in this same prepared source.
pub fn prepare(
    input: &[u8],
    environment: &mut Environment,
    lookup: impl FnMut(&[u8]) -> Option<Vec<u8>>,
) -> Result<Vec<Command>, Error> {
    prepare_located(input, environment, lookup, false)
        .map(|commands| commands.into_iter().map(|(command, _)| command).collect())
}

/// Parse with assignment effects disabled, matching source-file -n.
pub fn prepare_only(
    input: &[u8],
    environment: &mut Environment,
    lookup: impl FnMut(&[u8]) -> Option<Vec<u8>>,
) -> Result<Vec<Command>, Error> {
    prepare_located(input, environment, lookup, true)
        .map(|commands| commands.into_iter().map(|(command, _)| command).collect())
}

/// Preserve command source offsets for registration errors after preparation.
pub fn prepare_located(
    input: &[u8],
    environment: &mut Environment,
    lookup: impl FnMut(&[u8]) -> Option<Vec<u8>>,
    parse_only: bool,
) -> Result<Vec<(Command, usize)>, Error> {
    prepare_located_with_home(input, environment, lookup, parse_only, |_| None)
}

pub fn prepare_located_with_home(
    input: &[u8],
    environment: &mut Environment,
    mut lookup: impl FnMut(&[u8]) -> Option<Vec<u8>>,
    parse_only: bool,
    home: fn(&[u8]) -> Option<Vec<u8>>,
) -> Result<Vec<(Command, usize)>, Error> {
    let mut preparation = Preparation {
        input,
        reader: Reader::with_home(input, home),
        environment,
        lookup: &mut lookup,
        parse_only,
        positions: Vec::new(),
    };
    let commands = preparation.commands(false, true, 0)?;
    Ok(commands.into_iter().zip(preparation.positions).collect())
}

struct Branch {
    parent_active: bool,
    active: bool,
    taken: bool,
    saw_else: bool,
    offset: usize,
}

struct Preparation<'a, 'b, F> {
    input: &'a [u8],
    reader: Reader<'a>,
    environment: &'b mut Environment,
    lookup: &'b mut F,
    parse_only: bool,
    positions: Vec<usize>,
}

impl<F: FnMut(&[u8]) -> Option<Vec<u8>>> Preparation<'_, '_, F> {
    fn token(&mut self, condition: bool) -> Result<Option<Token>, Error> {
        let environment = &self.environment;
        let mut lookup = |name: &[u8]| environment.get(name).map(<[u8]>::to_vec);
        if condition {
            self.reader.next_condition(&mut lookup)
        } else {
            self.reader.next(&mut lookup)
        }
    }

    fn condition(&mut self, directive: usize) -> Result<bool, Error> {
        let token = self
            .token(true)?
            .ok_or_else(|| Error::at(self.input, directive, "missing conditional expression"))?;
        let TokenKind::Word(expression) = token.kind else {
            return Err(Error::at(
                self.input,
                token.offset,
                "expected conditional expression",
            ));
        };
        let value = tmuxx_format::expand_no_jobs(&expression, |key| {
            (self.lookup)(key).or_else(|| self.environment.get(key).map(<[u8]>::to_vec))
        })
        .map_err(|error| Error::at(self.input, token.offset, error.message))?;
        Ok(tmuxx_format::truthy(&value))
    }

    fn commands(
        &mut self,
        nested: bool,
        inherited_active: bool,
        depth: usize,
    ) -> Result<Vec<Command>, Error> {
        if depth > 100 {
            return Err(Error::at(
                self.input,
                self.reader.offset(),
                "command nesting limit exceeded",
            ));
        }
        let mut commands = Vec::new();
        let mut args = Vec::new();
        let mut blocks = std::collections::BTreeMap::new();
        let mut command_offset = 0;
        let mut branches: Vec<Branch> = Vec::new();
        while let Some(token) = self.token(false)? {
            let active = branches.last().map_or(inherited_active, |b| b.active);
            match token.kind {
                TokenKind::Word(word) => {
                    // Quoted/expanded directive-looking words are ordinary arguments.
                    let directive = self.input[token.offset] == b'%'
                        && word.get(1).is_some_and(u8::is_ascii_alphabetic);
                    if directive {
                        flush(
                            &mut commands,
                            &mut args,
                            &mut blocks,
                            active,
                            command_offset,
                            &mut self.positions,
                            depth == 0,
                        );
                        match word.as_slice() {
                            b"%if" => {
                                let condition = self.condition(token.offset)?;
                                branches.push(Branch {
                                    parent_active: active,
                                    active: active && condition,
                                    taken: condition,
                                    saw_else: false,
                                    offset: token.offset,
                                });
                            }
                            b"%elif" => {
                                if branches.is_empty() {
                                    return Err(Error::at(
                                        self.input,
                                        token.offset,
                                        "%elif without %if",
                                    ));
                                }
                                let condition = self.condition(token.offset)?;
                                let branch = branches.last_mut().unwrap();
                                if branch.saw_else {
                                    return Err(Error::at(
                                        self.input,
                                        token.offset,
                                        "%elif after %else",
                                    ));
                                }
                                branch.active = branch.parent_active && !branch.taken && condition;
                                branch.taken |= condition;
                            }
                            b"%else" => {
                                let branch = branches.last_mut().ok_or_else(|| {
                                    Error::at(self.input, token.offset, "%else without %if")
                                })?;
                                if branch.saw_else {
                                    return Err(Error::at(
                                        self.input,
                                        token.offset,
                                        "duplicate %else",
                                    ));
                                }
                                branch.active = branch.parent_active && !branch.taken;
                                branch.taken = true;
                                branch.saw_else = true;
                            }
                            b"%endif" => {
                                if branches.pop().is_none() {
                                    return Err(Error::at(
                                        self.input,
                                        token.offset,
                                        "%endif without %if",
                                    ));
                                }
                            }
                            b"%hidden" => {
                                let next = self.token(false)?.ok_or_else(|| {
                                    Error::at(
                                        self.input,
                                        token.offset,
                                        "%hidden requires an assignment",
                                    )
                                })?;
                                let TokenKind::Word(value) = next.kind else {
                                    return Err(Error::at(
                                        self.input,
                                        next.offset,
                                        "%hidden requires an assignment",
                                    ));
                                };
                                let (name, value) = assignment(&value).ok_or_else(|| {
                                    Error::at(
                                        self.input,
                                        next.offset,
                                        "%hidden requires an assignment",
                                    )
                                })?;
                                if active && !self.parse_only {
                                    self.environment.set(name.to_vec(), value.to_vec(), true);
                                }
                            }
                            _ => {
                                return Err(Error::at(
                                    self.input,
                                    token.offset,
                                    format!(
                                        "unsupported directive: {}",
                                        String::from_utf8_lossy(&word)
                                    ),
                                ));
                            }
                        }
                    } else if args.is_empty() && assignment(&word).is_some() {
                        let (name, value) = assignment(&word).unwrap();
                        if active && !self.parse_only {
                            self.environment.set(name.to_vec(), value.to_vec(), false);
                        }
                    } else {
                        if args.is_empty() {
                            command_offset = token.offset;
                        }
                        args.push(word);
                    }
                }
                TokenKind::OpenBrace => {
                    if args.is_empty() {
                        return Err(Error::at(
                            self.input,
                            token.offset,
                            "command group requires a command argument",
                        ));
                    }
                    let block = self.commands(true, active, depth + 1)?;
                    args.push(serialize(&block));
                    blocks.insert(args.len() - 1, block);
                }
                TokenKind::CloseBrace => {
                    if !nested {
                        return Err(Error::at(
                            self.input,
                            token.offset,
                            "unexpected closing brace",
                        ));
                    }
                    if let Some(branch) = branches.last() {
                        return Err(Error::at(self.input, branch.offset, "unterminated %if"));
                    }
                    flush(
                        &mut commands,
                        &mut args,
                        &mut blocks,
                        active,
                        command_offset,
                        &mut self.positions,
                        depth == 0,
                    );
                    return Ok(commands);
                }
                TokenKind::Separator | TokenKind::Newline => flush(
                    &mut commands,
                    &mut args,
                    &mut blocks,
                    active,
                    command_offset,
                    &mut self.positions,
                    depth == 0,
                ),
            }
        }
        if let Some(branch) = branches.last() {
            return Err(Error::at(self.input, branch.offset, "unterminated %if"));
        }
        if nested {
            return Err(Error::at(
                self.input,
                self.reader.offset(),
                "unterminated command group",
            ));
        }
        flush(
            &mut commands,
            &mut args,
            &mut blocks,
            inherited_active,
            command_offset,
            &mut self.positions,
            depth == 0,
        );
        Ok(commands)
    }
}

fn flush(
    commands: &mut Vec<Command>,
    args: &mut Vec<Vec<u8>>,
    blocks: &mut std::collections::BTreeMap<usize, Vec<Command>>,
    active: bool,
    offset: usize,
    positions: &mut Vec<usize>,
    record: bool,
) {
    if active && !args.is_empty() {
        if record {
            positions.push(offset);
        }
        commands.push(Command {
            args: std::mem::take(args),
            blocks: std::mem::take(blocks),
        });
    } else {
        args.clear();
        blocks.clear();
    }
}

fn assignment(word: &[u8]) -> Option<(&[u8], &[u8])> {
    let equals = word.iter().position(|&b| b == b'=')?;
    let name = &word[..equals];
    if !name.first().is_some_and(|&b| variable_start(b))
        || !name.iter().copied().all(variable_continue)
    {
        return None;
    }
    Some((name, &word[equals + 1..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assignments_prepare_before_commands_execute() {
        let mut env = Environment::default();
        let commands = prepare(
            b"A=first\nset-environment -g A runtime\ndisplay \"$A\"\nA=last\ndisplay \"${A}\"",
            &mut env,
            |_| None,
        )
        .unwrap();
        assert_eq!(commands.len(), 3);
        assert_eq!(commands[1].args[1], b"first");
        assert_eq!(commands[2].args[1], b"last");
        assert_eq!(env.get(b"A"), Some(b"last".as_slice()));
    }

    #[test]
    fn nested_conditions_only_prepare_first_matching_branch() {
        let mut env = Environment::default();
        let commands = prepare(b"A=before\n%if 0\nA=wrong\n%if 1\nA=also-wrong\n%endif\n%elif #{==:#{session_name},work}\nA=chosen\n%elif 1\nA=wrong-again\n%else\nA=wrong-last\n%endif\ndisplay $A", &mut env, |key| (key == b"session_name").then(|| b"work".to_vec())).unwrap();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].args[1], b"chosen");
    }

    #[test]
    fn inline_branches_hidden_variables_and_source_commands() {
        let mut env = Environment::default();
        let commands = prepare(b"%if 1 %hidden SECRET=value %else SECRET=wrong %endif\nsource-file '/not/read/here'\nif-shell true { display \"$SECRET\"; display '%if' }", &mut env, |_| None).unwrap();
        assert_eq!(env.get(b"SECRET"), Some(b"value".as_slice()));
        assert!(env.hidden.contains(b"SECRET".as_slice()));
        assert_eq!(commands[0].args[0], b"source-file");
        let nested = tmuxx_command::parse(&commands[1].args[2]).unwrap();
        assert_eq!(nested[0].args[1], b"value");
        assert_eq!(nested[1].args[1], b"%if");
        assert_eq!(commands[1].blocks[&2], nested);
    }

    #[test]
    fn queued_option_changes_do_not_change_preparation_context() {
        let commands = prepare(
            b"set-option -g @flag 1\n%if #{@flag}\ndisplay wrong\n%else\ndisplay initial\n%endif",
            &mut Environment::default(),
            |key| (key == b"@flag").then(|| b"0".to_vec()),
        )
        .unwrap();
        assert_eq!(commands[1].args[1], b"initial");
    }

    #[test]
    fn invalid_structure_is_an_error() {
        for input in [
            b"%else".as_slice(),
            b"%if 1",
            b"%hidden wrong",
            b"%if 1\n%else\n%elif 1\n%endif",
            b"%unknown",
        ] {
            assert!(
                prepare(input, &mut Environment::default(), |_| None).is_err(),
                "{input:?}"
            );
        }
    }
}
