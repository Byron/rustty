use std::collections::BTreeMap;
fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Parse tmux short options without ever reparsing command arguments as shell text.
pub struct Args {
    pub flags: BTreeMap<u8, Option<Vec<u8>>>,
    pub repeated: BTreeMap<u8, Vec<Option<Vec<u8>>>>,
    pub rest: Vec<Vec<u8>>,
}

impl Args {
    pub fn parse(input: &[Vec<u8>], spec: &str) -> Result<Self, String> {
        let spec = spec.as_bytes();
        let mut flags = BTreeMap::new();
        let mut repeated: BTreeMap<u8, Vec<Option<Vec<u8>>>> = BTreeMap::new();
        let mut i = 0;
        while i < input.len() {
            let word = &input[i];
            if word == b"--" {
                i += 1;
                break;
            }
            if word.len() < 2 || word[0] != b'-' {
                break;
            }
            let mut j = 1;
            while j < word.len() {
                let flag = word[j];
                if flag == b'?' {
                    return Err("usage requested".into());
                }
                if !flag.is_ascii_alphanumeric() {
                    return Err(format!("invalid flag -{}", flag as char));
                }
                let pos = spec
                    .iter()
                    .position(|f| *f == flag)
                    .ok_or_else(|| format!("unknown flag -{}", flag as char))?;
                if spec.get(pos + 1) == Some(&b':') {
                    if spec.get(pos + 2) == Some(&b':')
                        && j + 1 == word.len()
                        && input.get(i + 1).is_none_or(|next| {
                            next.first() == Some(&b'-')
                                && next
                                    .get(1)
                                    .is_some_and(|c| *c == b'-' || c.is_ascii_alphabetic())
                        })
                    {
                        flags.insert(flag, None);
                        repeated.entry(flag).or_default().push(None);
                        break;
                    }
                    let value = if j + 1 < word.len() {
                        word[j + 1..].to_vec()
                    } else {
                        i += 1;
                        input
                            .get(i)
                            .ok_or_else(|| format!("-{} expects an argument", flag as char))?
                            .clone()
                    };
                    repeated.entry(flag).or_default().push(Some(value.clone()));
                    flags.insert(flag, Some(value));
                    break;
                }
                flags.insert(flag, None);
                repeated.entry(flag).or_default().push(None);
                j += 1;
            }
            i += 1;
        }
        Ok(Self {
            flags,
            repeated,
            rest: input[i..].to_vec(),
        })
    }
    pub fn has(&self, c: u8) -> bool {
        self.flags.contains_key(&c)
    }
    pub fn get(&self, c: u8) -> Option<&[u8]> {
        self.flags.get(&c).and_then(|v| v.as_deref())
    }
    pub fn string(&self, c: u8) -> Option<String> {
        self.get(c).map(text)
    }
    pub fn number(&self, c: u8, default: u16) -> Result<u16, String> {
        self.get(c)
            .map(|s| {
                text(s)
                    .parse::<u16>()
                    .map_err(|_| format!("invalid size: {}", text(s)))
            })
            .unwrap_or(Ok(default))
            .and_then(|n| {
                if n > 0 {
                    Ok(n)
                } else {
                    Err("size must be positive".into())
                }
            })
    }
    pub fn no_rest(&self) -> Result<(), String> {
        if self.rest.is_empty() {
            Ok(())
        } else {
            Err("too many arguments".into())
        }
    }

    pub fn reject(&self, flags: &[u8]) -> Result<(), String> {
        if let Some(flag) = flags.iter().find(|flag| self.has(**flag)) {
            return Err(format!("unsupported flag -{}", *flag as char));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn optional_flag_values_preserve_following_flags_and_negative_numbers() {
        let argv = ["-D", "-L", "-5", "-R10", "-U", "--", "literal"].map(|v| v.as_bytes().to_vec());
        let args = Args::parse(&argv, "D::L::R::U::").unwrap();
        assert!(args.has(b'D'));
        assert_eq!(args.get(b'D'), None);
        assert_eq!(args.get(b'L'), Some(&b"-5"[..]));
        assert_eq!(args.get(b'R'), Some(&b"10"[..]));
        assert!(args.has(b'U'));
        assert_eq!(args.get(b'U'), None);
        assert_eq!(args.rest, [b"literal".to_vec()]);
        assert!(Args::parse(&[b"-D".to_vec()], "D::").unwrap().has(b'D'));
    }
}
