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

pub mod message;
pub use message::Message;
pub mod db;
pub use db::*;
pub mod case;
pub use case::*;
pub mod invocation;
pub use invocation::*;
pub mod connection;
pub use connection::*;

use std::{
    collections::{HashMap, HashSet},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::{mpsc, Mutex},
};

const SERVER_NAME: &str = "foxy-ircd";
const CAPABILITIES: &str = "message-tags cap-notify multi-prefix";

#[derive(Default)]
struct SessionState {
    nick: Option<String>,
    user: Option<String>,
    registered: bool,
}

#[derive(Default)]
struct ServerState {
    clients: HashMap<u64, mpsc::UnboundedSender<String>>,
    nicks: HashMap<String, u64>,
    channels: HashMap<String, HashSet<u64>>,
}

fn parse_irc_line(line: &str) -> (Option<&str>, String, Vec<String>) {
    let mut rest = line.trim();
    let mut tags = None;
    if rest.starts_with('@') {
        if let Some(idx) = rest.find(' ') {
            tags = Some(&rest[..idx]);
            rest = rest[idx + 1..].trim_start();
        }
    }
    if rest.is_empty() {
        return (tags, String::new(), Vec::new());
    }
    let mut args = Vec::new();
    let cmd;
    if let Some(idx) = rest.find(' ') {
        cmd = rest[..idx].to_ascii_uppercase();
        rest = rest[idx + 1..].trim_start();
    }
    else {
        cmd = rest.to_ascii_uppercase();
        rest = "";
    }
    while !rest.is_empty() {
        if let Some(stripped) = rest.strip_prefix(':') {
            args.push(stripped.to_owned());
            break;
        }
        if let Some(idx) = rest.find(' ') {
            args.push(rest[..idx].to_owned());
            rest = rest[idx + 1..].trim_start();
        }
        else {
            args.push(rest.to_owned());
            break;
        }
    }
    (tags, cmd, args)
}

fn prefixed_nick(state: &SessionState) -> String {
    format!(
        "{}!{}@localhost",
        state.nick.as_deref().unwrap_or("*"),
        state.user.as_deref().unwrap_or("unknown")
    )
}

fn send_to(tx: &mpsc::UnboundedSender<String>, msg: impl Into<String>) {
    let _ = tx.send(msg.into());
}

async fn register_if_ready(tx: &mpsc::UnboundedSender<String>,
                           session: &mut SessionState) {
    if session.registered || session.nick.is_none() || session.user.is_none() {
        return;
    }
    session.registered = true;
    let nick = session.nick.as_deref().unwrap_or("*");
    send_to(tx, format!(":{} 001 {} :Welcome to Foxy IRCd", SERVER_NAME, nick));
    send_to(tx, format!(":{} 002 {} :Your host is {}", SERVER_NAME, nick, SERVER_NAME));
    send_to(tx, format!(":{} 003 {} :This server was created recently", SERVER_NAME, nick));
    send_to(tx, format!(":{} 004 {} {} 0.1 ioCnosit kmn", SERVER_NAME, nick, SERVER_NAME));
}

async fn serve_connection(stream: Box<dyn FoxyStream + Send>,
                          shared_state: Arc<Mutex<ServerState>>,
                          client_id: u64) {
    let (read_half, mut write_half) = tokio::io::split(stream);
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();

    {
        let mut state = shared_state.lock().await;
        state.clients.insert(client_id, tx.clone());
    }

    let writer = tokio::spawn(async move {
        while let Some(line) = rx.recv().await {
            if write_half.write_all(line.as_bytes()).await.is_err() {
                break;
            }
            if write_half.write_all(b"\r\n").await.is_err() {
                break;
            }
        }
    });

    send_to(&tx, format!(":{} NOTICE * :Foxy IRCd ready", SERVER_NAME));

    let mut reader = BufReader::new(read_half);
    let mut buf = Vec::new();
    let mut session = SessionState::default();

    loop {
        buf.clear();
        let read = match reader.read_until(b'\n', &mut buf).await {
            Ok(x) => x,
            Err(_) => break,
        };
        if read == 0 {
            break;
        }
        while matches!(buf.last(), Some(b'\n' | b'\r')) {
            buf.pop();
        }
        let line = String::from_utf8_lossy(&buf).to_string();
        let (tags, cmd, args) = parse_irc_line(&line);
        if cmd.is_empty() {
            continue;
        }
        match cmd.as_str() {
            "CAP" => {
                if args.is_empty() {
                    send_to(&tx, format!(":{} 461 * CAP :Not enough parameters", SERVER_NAME));
                    continue;
                }
                let sub = args[0].to_ascii_uppercase();
                match sub.as_str() {
                    "LS" => send_to(
                        &tx,
                        format!(":{} CAP * LS :{}", SERVER_NAME, CAPABILITIES),
                    ),
                    "REQ" => {
                        if let Some(caps) = args.get(1) {
                            send_to(&tx, format!(":{} CAP * ACK :{}", SERVER_NAME, caps));
                        }
                        else {
                            send_to(&tx, format!(":{} 461 * CAP :Not enough parameters", SERVER_NAME));
                        }
                    },
                    "END" => (),
                    _ => send_to(&tx, format!(":{} CAP * NAK :{}", SERVER_NAME, args.get(1).map(|x| x.as_str()).unwrap_or(""))),
                }
            },
            "PING" => {
                let payload = args.get(0).map(|x| x.as_str()).unwrap_or(SERVER_NAME);
                send_to(&tx, format!("PONG :{}", payload));
            },
            "NICK" => {
                let nick = match args.get(0) {
                    Some(x) => x.clone(),
                    None => {
                        send_to(&tx, format!(":{} 431 * :No nickname given", SERVER_NAME));
                        continue;
                    },
                };
                let mut state = shared_state.lock().await;
                if let Some(owner) = state.nicks.get(&nick) {
                    if *owner != client_id {
                        send_to(&tx, format!(":{} 433 * {} :Nickname is already in use", SERVER_NAME, nick));
                        continue;
                    }
                }
                if let Some(old) = session.nick.replace(nick.clone()) {
                    state.nicks.remove(&old);
                }
                state.nicks.insert(nick, client_id);
                drop(state);
                register_if_ready(&tx, &mut session).await;
            },
            "USER" => {
                if args.len() < 4 {
                    send_to(&tx, format!(":{} 461 * USER :Not enough parameters", SERVER_NAME));
                    continue;
                }
                if session.registered {
                    send_to(&tx, format!(":{} 462 * :You may not reregister", SERVER_NAME));
                    continue;
                }
                session.user = Some(args[0].clone());
                register_if_ready(&tx, &mut session).await;
            },
            "JOIN" => {
                if !session.registered {
                    send_to(&tx, format!(":{} 451 * :You have not registered", SERVER_NAME));
                    continue;
                }
                let channel = match args.get(0) {
                    Some(x) => x.to_ascii_lowercase(),
                    None => {
                        send_to(&tx, format!(":{} 461 {} JOIN :Not enough parameters", SERVER_NAME, session.nick.as_deref().unwrap_or("*")));
                        continue;
                    },
                };
                let mut state = shared_state.lock().await;
                let chan_members = state.channels.entry(channel.clone()).or_insert_with(HashSet::new);
                chan_members.insert(client_id);
                let targets = chan_members.clone();
                let sender = prefixed_nick(&session);
                let msg = format!("{} :{} JOIN :{}", tags.unwrap_or(""), sender, channel).trim().to_owned();
                for id in targets {
                    if let Some(client_tx) = state.clients.get(&id) {
                        send_to(client_tx, msg.clone());
                    }
                }
            },
            "PRIVMSG" => {
                if !session.registered {
                    send_to(&tx, format!(":{} 451 * :You have not registered", SERVER_NAME));
                    continue;
                }
                if args.len() < 2 {
                    send_to(&tx, format!(":{} 461 {} PRIVMSG :Not enough parameters", SERVER_NAME, session.nick.as_deref().unwrap_or("*")));
                    continue;
                }
                let target = args[0].to_ascii_lowercase();
                let text = &args[1];
                let sender = prefixed_nick(&session);
                let raw = if let Some(tags) = tags {
                    format!("{} :{} PRIVMSG {} :{}", tags, sender, target, text)
                }
                else {
                    format!(":{} PRIVMSG {} :{}", sender, target, text)
                };
                let state = shared_state.lock().await;
                if target.starts_with('#') {
                    if let Some(members) = state.channels.get(&target) {
                        for id in members {
                            if *id == client_id {
                                continue;
                            }
                            if let Some(client_tx) = state.clients.get(id) {
                                send_to(client_tx, raw.clone());
                            }
                        }
                    }
                }
                else if let Some(id) = state.nicks.get(&target).copied() {
                    if let Some(client_tx) = state.clients.get(&id) {
                        send_to(client_tx, raw);
                    }
                }
                else {
                    send_to(&tx, format!(":{} 401 {} {} :No such nick/channel", SERVER_NAME, session.nick.as_deref().unwrap_or("*"), target));
                }
            },
            "QUIT" => break,
            _ => {
                send_to(
                    &tx,
                    format!(
                        ":{} 421 {} {} :Unknown command",
                        SERVER_NAME,
                        session.nick.as_deref().unwrap_or("*"),
                        cmd
                    ),
                );
            },
        }
    }

    {
        let mut state = shared_state.lock().await;
        if let Some(nick) = session.nick.take() {
            state.nicks.remove(&nick);
        }
        state.clients.remove(&client_id);
        for members in state.channels.values_mut() {
            members.remove(&client_id);
        }
    }

    drop(tx);
    let _ = writer.await;
}

fn main() {
    let state = Arc::new(Mutex::new(ServerState::default()));
    let next_id = Arc::new(AtomicU64::new(1));
    let Invocation { mut runtime }
    = match get_invocation(move |x| {
        let state = state.clone();
        let client_id = next_id.fetch_add(1, Ordering::Relaxed);
        if let Ok(addr) = x.peer_addr() {
            println!("{}", addr);
        }
        tokio::spawn(serve_connection(x, state, client_id));
    }) {
        Some(x) => x,
        None => std::process::exit(1),
    };
    let (mut send_quit, mut recv_quit) = tokio::sync::mpsc::channel(1);
    ctrlc::set_handler(move || {
        let _ = send_quit.try_send("control-C");
    }).unwrap();
    let reason = runtime.block_on(async {
        recv_quit.recv().await.unwrap()
    });
    eprintln!("\nShutting down server due to {}.", reason);
    // Try to be patient and let ongoing tasks finish, but don't block for more
    // than 15 seconds.
    runtime.shutdown_timeout(std::time::Duration::new(15, 0));
}
