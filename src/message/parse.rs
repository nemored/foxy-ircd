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

//! The functions in this module are either questions or parsers. Questions
//! are yes-or-no questions about a single character. Parsers attempt to parse
//! an input slice in a certain format, and return `Some(..., rest of line)` if
//! successful, `None` if invalid. Parsers parsing optional components, such
//! as the message tags, can also return `Some` or `None` *within* the outer
//! `Some`.

pub fn is_nulcrlf(x: u8) -> bool { x == 0 || x == b'\r' || x == b'\n' }
pub fn is_nulcrlfspace(x: u8) -> bool { x == 0 || x == b'\r' || x == b'\n'
                                        || x == b' '}
pub fn is_nulcrlfspaceatbang(x: u8) -> bool { x == 0 || x == b'\r'
                                              || x == b'\n' || x == b' '
                                              || x == b'@' || x == b'!'}

pub fn validate_param(param: &[u8]) -> Result<(), &'static str> {
    if param.is_empty() { Err("Invalid empty param") }
    else {
        match param.iter().find(|x| is_nulcrlfspace(**x)) {
            Some(_) => Err("Invalid byte in param"),
            None if param[0] == b':' => Err("Invalid colon in param"),
            None => Ok(()),
        }
    }
}

pub fn validate_trailing_param(param: &[u8]) -> Result<(), &'static str> {
    match param.iter().find(|x| is_nulcrlf(**x)) {
        Some(_) => Err("Invalid byte in param"),
        None => Ok(()),
    }
}

pub fn find_idx_of_space_or_end(line: &[u8]) -> Option<usize> {
    for n in 0 .. line.len() {
        match line[n] {
            b'\r' | 0 => return None,
            b' ' => return Some(n),
            _ => (),
        }
    }
    Some(line.len())
}

pub fn skip_leading_space(line: &[u8]) -> Option<&[u8]> {
    for n in 0 .. line.len() {
        match line[n] {
            b'\r' | 0 => return None,
            b' ' => (),
            _ => return Some(&line[n..])
        }
    }
    Some(&[])
}

pub fn parse_tags(line: &[u8]) -> Option<(Option<&[u8]>, &[u8])> {
    if line.is_empty() || line[0] != b'@' { Some((None, line)) }
    else {
        let split = find_idx_of_space_or_end(line)?;
        Some((Some(&line[1..split]), skip_leading_space(&line[split..])?))
    }
}

pub fn parse_source_name_or_nick(line: &[u8]) -> Option<(&[u8], u8, &[u8])> {
    for i in 0..line.len() {
        match line[i] {
            b'\r' | 0 => return None,
            b'@' | b'!' => return Some((&line[..i], line[i],
                                        &line[i+1..])),
            b' ' => unreachable!(), // space should not have made it this far
            _ => (),
        }
    }
    Some((&line[..], b' ', &[]))
}

pub fn parse_source_user(line: &[u8]) -> Option<(&[u8], u8, &[u8])> {
    for i in 0..line.len() {
        match line[i] {
            b'\r' | 0 | b'!' => return None,
            b'@' => return Some((&line[..i], line[i], &line[i+1..])),
            b' ' => unreachable!(), // space should not have made it this far
            _ => (),
        }
    }
    Some((&line[..], b' ', &[]))
}

pub fn parse_source_host(line: &[u8]) -> Option<(&[u8], &[u8])> {
    for i in 0..line.len() {
        match line[i] {
            b'\r' | 0 | b'!' | b'@' => return None,
            b' ' => unreachable!(), // space should not have made it this far
            _ => (),
        }
    }
    return Some((line, &[]))
}

pub fn parse_digit(digit: u8) -> Option<u32> {
    if digit >= b'0' && digit <= b'9' { Some((digit - b'0') as u32) }
    else { None }
}
