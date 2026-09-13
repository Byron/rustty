//! User-initiated clipboard pastes and text insertion share mode and safety policy.

use crate::{Terminal, clipboard};
use std::io::{self, Write};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Clipboard(clipboard::Location),
    Text,
}

pub type MimeReader<'a> = dyn FnMut(&[u8]) -> io::Result<Vec<u8>> + 'a;
pub type SecureRandom<'a> = dyn FnMut(&mut [u8]) -> io::Result<()> + 'a;

pub enum Contents<'a> {
    Memory(&'a [clipboard::Content]),
    /// A paste event reads nothing; text insertion reads only the first text MIME.
    Reader {
        mimes: &'a [Vec<u8>],
        read: &'a mut MimeReader<'a>,
    },
}

pub struct Request<'a> {
    pub source: Source,
    pub contents: Contents<'a>,
    pub allow_unsafe: bool,
}

#[derive(Debug)]
pub enum Error {
    UnsafePaste,
    ReadFailed(io::Error),
    EntropyUnavailable(io::Error),
    WriteFailed(io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsafePaste => f.write_str("paste requires confirmation"),
            Self::ReadFailed(error) => write!(f, "could not read paste: {error}"),
            Self::EntropyUnavailable(error) => {
                write!(f, "secure paste entropy unavailable: {error}")
            }
            Self::WriteFailed(error) => write!(f, "could not write paste: {error}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::UnsafePaste => None,
            Self::ReadFailed(error)
            | Self::EntropyUnavailable(error)
            | Self::WriteFailed(error) => Some(error),
        }
    }
}

impl Terminal {
    /// Write a paste, returning false if there is no nonempty text to insert.
    /// Supply secure randomness only when the host can serve subsequent Kitty
    /// clipboard reads. Otherwise mode 5522 falls back to text. Entropy and
    /// read failures write nothing; a failed event write revokes its grant.
    pub fn paste(
        &mut self,
        request: Request<'_>,
        secure_random: Option<&mut SecureRandom<'_>>,
        output: &mut dyn Write,
    ) -> Result<bool, Error> {
        if let Source::Clipboard(location) = request.source
            && self.modes.dec(5522)
            && let Some(random) = secure_random
        {
            let password = one_time_password(random).map_err(Error::EntropyUnavailable)?;
            let mimes: Vec<&[u8]> = match &request.contents {
                Contents::Memory(contents) => contents
                    .iter()
                    .take(16)
                    .map(|c| c.mime.as_slice())
                    .collect(),
                Contents::Reader { mimes, .. } => {
                    mimes.iter().take(16).map(Vec::as_slice).collect()
                }
            };
            let bytes = clipboard::kitty::paste_event(location, &password, &mimes);
            self.clipboard.grant(&password, true, true);
            if let Err(error) = output.write_all(&bytes) {
                self.clipboard.use_grant(&password, true);
                return Err(Error::WriteFailed(error));
            }
            return Ok(true);
        }

        let owned;
        let text = match request.contents {
            Contents::Memory(contents) => {
                let Some(content) = contents.iter().find(|c| clipboard::is_text_mime(&c.mime))
                else {
                    return Ok(false);
                };
                content.data.as_ref()
            }
            Contents::Reader { mimes, read } => {
                let Some(mime) = mimes.iter().find(|m| clipboard::is_text_mime(m)) else {
                    return Ok(false);
                };
                owned = read(mime).map_err(Error::ReadFailed)?;
                &owned
            }
        };
        if text.is_empty() {
            return Ok(false);
        }
        if !request.allow_unsafe && !self.paste_is_safe(text) {
            return Err(Error::UnsafePaste);
        }
        output
            .write_all(&self.encode_paste(text))
            .map_err(Error::WriteFailed)?;
        Ok(true)
    }
}

fn one_time_password(random: &mut SecureRandom<'_>) -> io::Result<[u8; 22]> {
    const ALPHABET: &[u8] = b"23456789abcdefghijkmnopqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ";
    let mut password = [0; 22];
    let mut filled = 0;
    while filled < password.len() {
        let mut bytes = [0; 44];
        random(&mut bytes)?;
        for byte in bytes {
            if usize::from(byte) >= 256 / ALPHABET.len() * ALPHABET.len() {
                continue;
            }
            password[filled] = ALPHABET[usize::from(byte) % ALPHABET.len()];
            filled += 1;
            if filled == password.len() {
                break;
            }
        }
    }
    Ok(password)
}
