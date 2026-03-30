/*
 * This file is part of Foxy IRCd, copyright ©2020 Solra Bizna.
 *
 * Foxy IRCd is free software: you can redistribute it and/or modify it under
 * the terms of the GNU General Public License as published by the Free
 * Software Foundation, either version 3 of the License, or (at your option)
 * any later version.
 *
 * Foxy IRCd is distributed in the hope that it will be useful, but WITHOUT ANY
 * WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS
 * FOR A PARTICULAR PURPOSE. See the GNU General Public License for more
 * details.
 *
 * You should have received a copy of the GNU General Public License along with
 * Foxy IRCd. If not, see <https://www.gnu.org/licenses/>.
 */

use std::{
    convert::TryInto,
    fmt::{Debug, Formatter},
    hash::{Hash, Hasher},
    ops::Range,
};
use arrayref::array_ref;
use crate::*;

mod parse;
use parse::*;

/// Copy some bytes into a buffer, and return the `Range` occupied.
///
/// Note: Like the other ranges in this file, this is a range of `u32`, not
/// `usize`. IRC messages are normally limited to **510 bytes**, so a 4GiB
/// message is purely ridiculous.
fn inter_bytes(buf: &mut Vec<u8>, bytes: &[u8]) -> Range<u32> {
    let staato = buf.len() as u32;
    buf.extend_from_slice(bytes);
    let owari = buf.len() as u32;
    staato .. owari
}

/// Extract a `Range<u32>` from a slice.
fn extract_bytes<'a>(buf: &'a [u8], range: &Range<u32>) -> &'a [u8] {
    &buf[range.start as usize .. range.end as usize]
}

/// The internal version of `Source`. Refers to its data by `Range`.
enum IntSource {
    Server { name: Range<u32> },
    Client { nick: Range<u32>, user: Option<Range<u32>>, host: Range<u32> },
}

impl IntSource {
    /// Borrow this `IntSource` for outside use.
    fn extract<'a>(&self, buf: &'a[u8]) -> Source<'a> {
        match self {
            IntSource::Server { name } => Source::Server {
                name: extract_bytes(buf, name),
            },
            IntSource::Client { nick, user, host } => Source::Client {
                nick: extract_bytes(buf, nick),
                user: user.as_ref().map(|x| extract_bytes(buf, x)),
                host: extract_bytes(buf, host),
            },
        }
    }
}

/// The source (AKA prefix) of a message.
#[derive(PartialEq,Eq,PartialOrd,Ord)]
pub enum Source<'a> {
    /// Message came from a server.
    Server { name: &'a[u8] },
    /// Message came from a client.
    Client { nick: &'a[u8], user: Option<&'a[u8]>, host: &'a[u8] },
}

impl<'a> Source<'a> {
    /// The number of bytes this Source would require to encode into a message,
    /// including leading colon and trailing space.
    fn raw_len(&self) -> usize {
        match self {
            &Source::Server { name } => name.len() + 2,
            &Source::Client { nick, user: None, host }
            => nick.len() + host.len() + 3,
            &Source::Client { nick, user: Some(user), host }
            => nick.len() + user.len() + host.len() + 4,
        }
    }
    /// Encodes this Source into a message, and returns its `IntSource`
    /// equivalent.
    fn inter(&self, buf: &mut Vec<u8>) -> IntSource {
        buf.push(b':');
        match self {
            &Source::Server { name } => {
                let name = inter_bytes(buf, name);
                buf.push(b' ');
                IntSource::Server { name }
            },
            &Source::Client { nick, user, host } => {
                let nick = inter_bytes(buf, nick);
                let user = user.map(|user| {
                    buf.push(b'!');
                    inter_bytes(buf, user)
                });
                buf.push(b'@');
                let host = inter_bytes(buf, host);
                buf.push(b' ');
                IntSource::Client { nick, user, host }
            },
        }
    }
    /// Validates this source, ensuring that it can be sent in a Message. Very
    /// lax; only checks for stray NUL, CR, LF, space, @, and !.
    fn validate(&self) -> Result<(), &'static str> {
        match self {
            Source::Server { name } => {
                if name.iter().find(|x| is_nulcrlfspaceatbang(**x)).is_some() {
                    Err("invalid character in server prefix")
                }
                else {
                    Ok(())
                }
            },
            Source::Client { nick, user, host } => {
                if nick.iter()
                    .find(|x| is_nulcrlfspaceatbang(**x)).is_some() {
                    Err("invalid character in client nickname")
                }
                else if user.unwrap_or(b"").iter()
                    .find(|x| is_nulcrlfspaceatbang(**x))
                    .is_some() {
                    Err("invalid character in client username")
                }
                else if host.iter()
                    .find(|x| is_nulcrlfspaceatbang(**x)).is_some() {
                    Err("invalid character in client hostname")
                }
                else {
                    Ok(())
                }
            },
        }
    }
    /// Parse part of a raw message into a `Source`, or determine that it lacks
    /// a `Source`.
    fn parse(line: &[u8]) -> Option<(Option<Source>, &[u8])> {
        if line.is_empty() || line[0] != b':' { Some((None, line)) }
        else {
            let split = find_idx_of_space_or_end(line)?;
            let rest = skip_leading_space(&line[split..])?;
            let (first, finale, line)
                = parse_source_name_or_nick(&line[1..split])?;
            let (second, finale, line) = match finale {
                b' ' => {
                    debug_assert!(line.is_empty());
                    return Some((Some(Source::Server { name: first }),
                                 rest))
                },
                b'!' => {
                    let (second, finale, line)
                        = parse_source_user(line)?;
                    (Some(second), finale, line)
                },
                _ => {
                    debug_assert!(finale == b'@');
                    (None, finale, line)
                },
            };
            if finale != b'@' { None }
            else {
                let (host, line) = parse_source_host(line)?;
                debug_assert!(line.is_empty());
                Some((Some(Source::Client { nick: first, user: second, host }),
                      rest))
            }
        }
    }
}

impl<'a> Debug for Source<'a> {
    fn fmt(&self, fmt: &mut Formatter) -> Result<(), std::fmt::Error> {
        match self {
            Source::Server { name } => {
                fmt.write_str("Source::Server { name: ")?;
                Debug::fmt(&String::from_utf8_lossy(name), fmt)?;
            },
            Source::Client { nick, user, host } => {
                fmt.write_str("Source::Client { nick: ")?;
                Debug::fmt(&String::from_utf8_lossy(nick), fmt)?;
                fmt.write_str(", user: ")?;
                Debug::fmt(&user.map(|x| String::from_utf8_lossy(x)), fmt)?;
                fmt.write_str(", host: ")?;
                Debug::fmt(&String::from_utf8_lossy(host), fmt)?;
            },
        }
        fmt.write_str(" }")
    }
}

/// The internal version of `Command`. Refers to its data by `Range`.
enum IntCommand {
    Numeric(u32, Range<u32>),
    Textual(Range<u32>),
}

impl IntCommand {
    /// Borrow this `IntCommand` for outside use.
    fn extract<'a>(&self, buf: &'a[u8]) -> Command<'a> {
        match self {
            IntCommand::Numeric(x, _) => Command::Numeric(*x),
            IntCommand::Textual(x) => Command::Textual(extract_bytes(buf, x)),
        }
    }
}

/// A command component of a message.
#[derive(PartialEq,Eq,PartialOrd,Ord)]
pub enum Command<'a> {
    /// A numeric command (e.g. 375 = the start of the MOTD)
    Numeric(u32),
    /// A textual command. This will *always* have been folded to uppercase.
    Textual(&'a[u8]),
}

impl<'a> Command<'a> {
    /// Encodes this Source into a buffer. Intermediate step before `inter`
    /// can be called. Folds case and checks validity.
    fn bufferize(&self) -> Result<Vec<u8>, &'static str> {
        match self {
            &Command::Numeric(x) if x == 0 || x > 999
                => Err("Invalid command number"),
            &Command::Numeric(x) => Ok(format!("{:03}",x).into_bytes()),
            &Command::Textual(x) => Ok({
                if x.iter().find(|x| is_nulcrlfspace(**x)).is_some() {
                    Err("invalid character in command name")?
                }
                let mut buf = x.to_owned();
                for q in buf.iter_mut() { *q = upcase(*q) }
                buf
            })
        }
    }
    /// Encodes this Source into a message, and returns its `IntCommand`
    /// equivalent.
    ///
    /// Weird implementation detail: assumes that it has already been folded
    /// and bufferized into the provided buffer.
    fn inter(&self, me_buf: Vec<u8>, out_buf: &mut Vec<u8>) -> IntCommand {
        let range = inter_bytes(out_buf, &me_buf[..]);
        match self {
            &Command::Numeric(x) =>
                IntCommand::Numeric(x, range),
            &Command::Textual(_) =>
                IntCommand::Textual(range),
        }
    }
    /// Parse part of a raw message into a `Command`.
    fn parse(line: &[u8]) -> Option<(Command, &[u8])> {
        if line.is_empty() { None }
        else {
            let split = find_idx_of_space_or_end(line)?;
            let rest = skip_leading_space(&line[split..])?;
            let line = &line[..split];
            if line.len() == 3 {
                let (a,b,c) = (parse_digit(line[0]),
                               parse_digit(line[1]),
                               parse_digit(line[2]));
                match (a,b,c) {
                    (Some(a), Some(b), Some(c)) =>
                        return Some((Command::Numeric(a*100+b*10+c), rest)),
                    _ => (),
                }
            }
            Some((Command::Textual(line), rest))
        }
    }
}

impl<'a> Debug for Command<'a> {
    fn fmt(&self, fmt: &mut Formatter) -> Result<(), std::fmt::Error> {
        match self {
            Command::Numeric(x) =>
                fmt.write_str(&format!("Command::Numeric({:03})", *x)),
            Command::Textual(x) => {
                fmt.write_str("Command::Textual(")?;
                Debug::fmt(&String::from_utf8_lossy(x), fmt)?;
                fmt.write_str(")")
            },
        }
    }
}

pub struct Message {
    buf: Vec<u8>,
    source: Option<IntSource>,
    command: IntCommand,
    param_data_range: Range<u32>,
    raw_message_len: u32, // the part of buf that isn't param_data array
    trailer: bool,
}

/// Encapsulates an RFC 1459 message. Holds a single buffer which, among other
/// things, contains the message in encoded form. Can efficiently return slices
/// to individual components of the message, or a slice to the message as it
/// should be sent over the wire.
impl Message {
    /// Parse an input line into a `Message`. The line must have had its
    /// newline stripped, as well as its optional carriage return. The caller
    /// must detect and skip an empty message.
    pub fn parse(line: &[u8]) -> Option<Message> {
        let (_tags, line) = parse_tags(line)?;
        let (source, line) = Source::parse(line)?;
        let (command, mut line) = Command::parse(line)?;
        let mut params = Vec::new();
        let mut trailer = false;
        while !line.is_empty() {
            if line[0] == b':' {
                params.push(&line[1..]);
                trailer = true;
                break
            }
            let split = find_idx_of_space_or_end(line)?;
            params.push(&line[..split]);
            line = skip_leading_space(&line[split..])?;
        }
        Some(Message::assemble(source.as_ref(), &command, &params[..], trailer)
             .unwrap())
    }
    /// Makes a new `Message` from provided component parts.
    pub fn assemble(source: Option<&Source>, command: &Command,
                    params: &[&[u8]], trailer: bool)
                    -> Result<Message, &'static str> {
        // At runtime, if this assertion doesn't hold, our calculated message
        // length will be one byte too long. Since this costs at most 8 bytes,
        // and we're already wasting up to 7 bytes on a message that has no
        // params anyway, this isn't worth checking for in a release build.
        debug_assert!(!(trailer && params.is_empty()));
        if let Some(source) = source {
            source.validate()?;
        }
        let command_buf = command.bufferize()?;
        let message_len =
            source.map(|x| x.raw_len()).unwrap_or(0)
            + command_buf.len()
            + params.iter().map(|x| x.len() + 1).fold(0, |a,b| a+b)
            + if trailer { 3 } else { 2 };
        let param_base = (message_len + 7) & !7;
        let buf_len = param_base + params.len() * 8;
        let _buf_len_as_u32: u32 = match buf_len.try_into() {
            Ok(x) => x,
            Err(_) => panic!("Message over 4GiB long! Absurd!"),
        };
        let mut buf = Vec::with_capacity(buf_len);
        let interred_source = source.map(|x| x.inter(&mut buf));
        let interred_command = command.inter(command_buf, &mut buf);
        let mut interred_params = Vec::with_capacity(params.len());
        for n in 0..params.len() {
            let param = params[n];
            buf.push(b' ');
            if n == params.len() - 1 && trailer {
                buf.push(b':');
                validate_trailing_param(param)?;
            }
            else {
                validate_param(param)?;
            }
            interred_params.push(inter_bytes(&mut buf, param));
        }
        buf.push(b'\r');
        buf.push(b'\n');
        assert_eq!(buf.len(), message_len);
        buf.resize(param_base, 0);
        for x in interred_params.into_iter() {
            buf.extend_from_slice(&x.start.to_ne_bytes()[..]);
            buf.extend_from_slice(&x.end.to_ne_bytes()[..]);
        }
        assert_eq!(buf.len(), buf_len);
        Ok(Message {
            buf: buf,
            source: interred_source,
            command: interred_command,
            param_data_range: param_base as u32 .. buf_len as u32,
            raw_message_len: message_len as u32,
            trailer,
        })
    }
    /// Returns the exact bytes to send over the wire to transmit this
    /// message. Includes the trailing `"\r\n"`.
    pub fn get_raw(&self) -> &[u8] {
        &self.buf[.. self.raw_message_len as usize]
    }
    /// Returns the source (AKA prefix) specification of the message, if any.
    pub fn get_source(&self) -> Option<Source> {
        self.source.as_ref().map(|x| x.extract(&self.buf[..]))
    }
    /// Returns the command for this message.
    pub fn get_command(&self) -> Command {
        self.command.extract(&self.buf[..])
    }
    /// Returns the number of additional parameters in this message.
    pub fn get_param_count(&self) -> u32 {
        (self.param_data_range.len() / 8) as u32
    }
    /// Returns the nth parameter.
    pub fn get_nth_param(&self, n: u32) -> Option<&[u8]> {
        let param_list = extract_bytes(&self.buf[..], &self.param_data_range);
        if n >= (param_list.len() >> 3) as u32 { None }
        else {
            let raw_range_start = array_ref![param_list, (n * 8) as usize, 4];
            let raw_range_end = array_ref![param_list,(n * 8) as usize + 4, 4];
            Some(extract_bytes(&self.buf[..],
                               &(u32::from_ne_bytes(*raw_range_start)
                                 ..u32::from_ne_bytes(*raw_range_end))))
        }
    }
    /// Returns whether the last parameter in this message follows a colon.
    /// **YOU MUST NOT USE THIS INFORMATION TO CHANGE HOW YOU HANDLE AN
    /// INCOMING MESSAGE!**
    pub fn has_trailer(&self) -> bool {
        self.trailer
    }
}

impl Hash for Message {
    fn hash<H: Hasher>(&self, h: &mut H) {
        // Only hashing this part of `buf` is required, since it fully
        // specifies the message.
        (&self.buf[.. self.raw_message_len as usize]).hash(h);
    }
}

impl Debug for Message {
    fn fmt(&self, fmt: &mut Formatter) -> Result<(), std::fmt::Error> {
        let s = String::from_utf8_lossy(&self.buf[.. self.raw_message_len as usize]);
        Debug::fmt(&s, fmt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Test {
        name: &'static str,
        raw: &'static [u8],
        source: Option<Source<'static>>,
        command: Command<'static>,
        params: &'static [&'static [u8]],
        trailer: bool,
    }
    const TESTS: &[Test] = &[
        Test {
            name: "Simple Numeric",
            raw: b"314\r\n",
            source: None,
            command: Command::Numeric(314),
            params: &[],
            trailer: false,
        },
        Test {
            name: "Simple Textual",
            raw: b"FOO\r\n",
            source: None,
            command: Command::Textual(b"FOO"),
            params: &[],
            trailer: false,
        },
        Test {
            name: "Prefixed, Trailer",
            raw: b":irc.example.com 314 TestDood :This is a simple test\r\n",
            source: Some(Source::Server { name: b"irc.example.com" }),
            command: Command::Numeric(314),
            params: &[
                b"TestDood",
                b"This is a simple test",
            ],
            trailer: true
        },
        Test {
            name: "Mega Trip",
            raw: b":nickName!user@HostName PRIVMSG #not-invalid:name :Eek, a \
                   colon!\r\n",
            source: Some(Source::Client { nick: b"nickName",
                                          user: Some(b"user"),
                                          host: b"HostName" }),
            command: Command::Textual(b"PRIVMSG"),
            params: &[
                b"#not-invalid:name",
                b"Eek, a colon!",
            ],
            trailer: true
        },
    ];
    const BAD_ASSEMBLIES: &[Test] = &[
        Test {
            name: "Bad Source Server",
            raw: b"",
            source: Some(Source::Server { name: b"impossible!server" }),
            command: Command::Textual(b"FOO"),
            params: &[
                b"foo",
                b"foo foo",
            ],
            trailer: true,
        },
        Test {
            name: "Bad Source Nick",
            raw: b"",
            source: Some(Source::Client { nick: b"Nick Wilde",
                                          user: None,
                                          host: b"topia.zoo" }),
            command: Command::Textual(b"FOO"),
            params: &[
                b"foo",
                b"foo foo",
            ],
            trailer: true,
        },
        Test {
            name: "Bad Source User",
            raw: b"",
            source: Some(Source::Client { nick: b"NickWilde",
                                          user: Some(b"n!wilde"),
                                          host: b"topia.zoo" }),
            command: Command::Textual(b"FOO"),
            params: &[
                b"foo",
                b"foo foo",
            ],
            trailer: true,
        },
        Test {
            name: "Bad Source Host",
            raw: b"",
            source: Some(Source::Client { nick: b"NickWilde",
                                          user: None,
                                          host: b"nwilde@police.zoo" }),
            command: Command::Textual(b"FOO"),
            params: &[
                b"foo",
                b"foo foo",
            ],
            trailer: true,
        },
        Test {
            name: "Bad Command",
            raw: b"",
            source: Some(Source::Client { nick: b"NickWilde",
                                          user: None,
                                          host: b"topia.zoo" }),
            command: Command::Textual(b"FO O"),
            params: &[
                b"foo",
                b"foo foo",
            ],
            trailer: true,
        },
        Test {
            name: "Bad Param",
            raw: b"",
            source: Some(Source::Client { nick: b"NickWilde",
                                          user: None,
                                          host: b"topia.zoo" }),
            command: Command::Textual(b"FOO"),
            params: &[
                b":foo",
                b"foo foo",
            ],
            trailer: true,
        },
        Test {
            name: "Missing Trailer",
            raw: b"",
            source: Some(Source::Client { nick: b"NickWilde",
                                          user: None,
                                          host: b"topia.zoo" }),
            command: Command::Textual(b"FOO"),
            params: &[
                b"foo",
                b"foo foo",
            ],
            trailer: false,
        },
    ];
    #[test]
    pub fn assemble() {
        for test in TESTS {
            let message = Message::assemble(test.source.as_ref(),
                                            &test.command,
                                            test.params,
                                            test.trailer).unwrap();
            let mut problems =
                (if test.source == message.get_source() { 0 }
                 else {
                     eprintln!("Sources don't match!");
                     1
                 }) +
                (if test.command == message.get_command() { 0 }
                 else {
                     eprintln!("Commands don't match!");
                     1
                 }) +
                (if test.trailer == message.has_trailer() { 0 }
                 else {
                     eprintln!("has_trailer doesn't match!");
                     1
                 });
            if message.get_param_count() as usize != test.params.len() {
                problems += 1;
                eprintln!("Wrong number of params!");
            }
            else {
                for n in 0 .. test.params.len() {
                    if message.get_nth_param(n as u32).unwrap() != test.params[n] {
                        eprintln!("Wrong param!");
                        problems += 1;
                    }
                }
                assert!(message.get_nth_param(message.get_param_count())
                        .is_none());
            }
            if problems > 0 {
                eprintln!("Expected:");
                eprintln!("\traw: {:?}", String::from_utf8_lossy(test.raw));
                eprintln!("\tsource: {:?}", test.source);
                eprintln!("\tcommand: {:?}", test.command);
                for n in 0..test.params.len() {
                    if n == (test.params.len()-1) && test.trailer {
                        eprintln!("\t\t(trailer)");
                    }
                    eprintln!("\tparams[{}]: {:?}", n,
                              String::from_utf8_lossy(test.params[n]));
                }
                eprintln!("Got:");
                eprintln!("\traw: {:?}", String::from_utf8_lossy(message.get_raw()));
                eprintln!("\tsource: {:?}", message.get_source());
                eprintln!("\tcommand: {:?}", message.get_command());
                for n in 0..message.get_param_count() {
                    if n == (message.get_param_count()-1) && message.has_trailer() {
                        eprintln!("\t\t(trailer)");
                    }
                    eprintln!("\tparams[{}]: {:?}", n,
                              String::from_utf8_lossy(message.get_nth_param(n).unwrap()));
                }
                panic!("Assembly test {:?} failed!", test.name);
            }
        }
    }
    #[test]
    pub fn bad_assembly() {
        for test in BAD_ASSEMBLIES {
            let message = Message::assemble(test.source.as_ref(),
                                            &test.command,
                                            test.params,
                                            test.trailer);
            if message.is_ok() {
                eprintln!("Test that should have failed:");
                eprintln!("\traw: {:?}", String::from_utf8_lossy(test.raw));
                eprintln!("\tsource: {:?}", test.source);
                eprintln!("\tcommand: {:?}", test.command);
                for n in 0..test.params.len() {
                    if n == (test.params.len()-1) && test.trailer {
                        eprintln!("\t\t(trailer)");
                    }
                    eprintln!("\tparams[{}]: {:?}", n,
                              String::from_utf8_lossy(test.params[n]));
                }
                panic!("Bad assembly test {:?} failed!", test.name);
            }
        }
    }
    #[test]
    pub fn parse() {
        for test in TESTS {
            let message = Message::parse(&test.raw[..test.raw.len()-2])
                .unwrap();
            let mut problems =
                (if test.source == message.get_source() { 0 }
                 else {
                     eprintln!("Sources don't match!");
                     1
                 }) +
                (if test.command == message.get_command() { 0 }
                 else {
                     eprintln!("Commands don't match!");
                     1
                 }) +
                (if test.trailer == message.has_trailer() { 0 }
                 else {
                     eprintln!("has_trailer doesn't match!");
                     1
                 });
            if message.get_param_count() as usize != test.params.len() {
                problems += 1;
                eprintln!("Wrong number of params!");
            }
            else {
                for n in 0 .. test.params.len() {
                    if message.get_nth_param(n as u32).unwrap() != test.params[n] {
                        eprintln!("Wrong param!");
                        problems += 1;
                    }
                }
                assert!(message.get_nth_param(message.get_param_count())
                        .is_none());
            }
            if problems > 0 {
                eprintln!("Expected:");
                eprintln!("\traw: {:?}", String::from_utf8_lossy(test.raw));
                eprintln!("\tsource: {:?}", test.source);
                eprintln!("\tcommand: {:?}", test.command);
                for n in 0..test.params.len() {
                    if n == (test.params.len()-1) && test.trailer {
                        eprintln!("\t\t(trailer)");
                    }
                    eprintln!("\tparams[{}]: {:?}", n,
                              String::from_utf8_lossy(test.params[n]));
                }
                eprintln!("Got:");
                eprintln!("\traw: {:?}", String::from_utf8_lossy(message.get_raw()));
                eprintln!("\tsource: {:?}", message.get_source());
                eprintln!("\tcommand: {:?}", message.get_command());
                for n in 0..message.get_param_count() {
                    if n == (message.get_param_count()-1) && message.has_trailer() {
                        eprintln!("\t\t(trailer)");
                    }
                    eprintln!("\tparams[{}]: {:?}", n,
                              String::from_utf8_lossy(message.get_nth_param(n).unwrap()));
                }
                panic!("Parse test {:?} failed!", test.name);
            }
        }
    }
}
