use crate::*;

use std::net::SocketAddr;

pub struct Invocation {
    pub runtime: tokio::runtime::Runtime,
}

fn print_usage(program_name: &str, opts: getopts::Options) {
    let brief = format!(r#"
Usage: {} options...

Foxy IRCd is IRC server software written in Rust."#, program_name);
    print!(r#"{}
If NO -l options are given, the default is:

  -l [::]:6667
"#, opts.usage(&brief));
    print!(r#"
TLS listeners can be requested with -s/--listen-tls but require a future TLS
backend integration.
"#);
}

pub fn get_invocation<I>(incoming_connection_handler: I)
                         -> Option<Invocation>
where I: FnMut(Box<dyn FoxyStream + Send>) + Clone + Send + 'static {
    let mut opts = getopts::Options::new();
    opts.optflag("h", "help", ""); // heh
    opts.optflag("?", "usage", "Print what you're reading now.");
    opts.optmulti("l", "listen", "Listen for non-TLS connections on a given \
                                  address and port. May be given more than \
                                  once.", "ADDR:PORT");
    opts.optmulti("s", "listen-tls", "Listen for TLS connections on a given \
                                      address and port. May be given more \
                                      than once.", "ADDR:PORT");
    opts.optmulti("d", "db-dir", "Specify a directory to use as a database. \
                                  If given more than once, they are in \
                                  descending order of priority, and only the \
                                  first one will be written to.", "PATH");
    opts.optopt("t", "threads", "Specify the number of reactor threads to \
                                 use.", "NUM | \"auto\" (default 1)");
    let args: Vec<String> = std::env::args().collect();
    let program_name = args.get(0).map(|x| x.as_str()).unwrap_or("foxy_ircd");
    if args.len() <= 1 {
        println!("At least one argument is required. If you really mean to \
                  start with the default listeners and runtime, and no \
                  database, pass '--' as the only argument.");
        print_usage(program_name, opts);
        return None
    }
    let matches = match opts.parse(&args[1..]) {
        Ok(x) => x,
        Err(x) => {
            println!("{}", x);
            print_usage(program_name, opts);
            return None
        }
    };
    if matches.opt_present("?") || matches.opt_present("h") {
        print_usage(program_name, opts);
        return None
    }
    // keep this around...
    let wanted_threads = matches.opt_str("t");
    // ...to borrow here.
    let wanted_threads = match wanted_threads.as_ref().map(|x| x.as_str()) {
        None | Some("1") => 1,
        Some("auto") => num_cpus::get(),
        Some(x) => match x.parse() {
            Err(_) | Ok(0) => {
                println!("Invalid number of threads specified.");
                print_usage(program_name, opts);
                return None
            },
            Ok(x) => x,
        },
    };
    let mut builder = tokio::runtime::Builder::new();
    let runtime = match wanted_threads {
        1 => builder.basic_scheduler(),
        wanted_threads => builder.threaded_scheduler()
            .core_threads(wanted_threads),
    }.enable_io().build().unwrap();
    let mut listeners = Vec::new();
    if !matches.opt_present("l") && !matches.opt_present("s") {
        listeners.push((("[::]:6667").parse().unwrap(), false));
    }
    for el in matches.opt_strs("l") {
        let addr: SocketAddr = match el.parse() {
            Ok(x) => x,
            Err(_) => {
                println!("Invalid IP address+host: {}", el);
                print_usage(program_name, opts);
                return None
            },
        };
        listeners.push((addr, false))
    }
    for el in matches.opt_strs("s") {
        let _addr: SocketAddr = match el.parse() {
            Ok(x) => x,
            Err(_) => {
                println!("Invalid IP address+host: {}", el);
                print_usage(program_name, opts);
                return None
            },
        };
        eprintln!("TLS listener {} requested, but TLS support is not yet enabled.", el);
        return None
    }
    if !runtime.enter(|| {
        for (addr, _tls) in listeners.into_iter() {
            let listener = match std::net::TcpListener::bind(addr) {
                Ok(x) => x,
                Err(x) => {
                    eprintln!("Unable to bind to {}: {}", addr, x);
                    return false
                },
            };
            let mut listener = tokio::net::TcpListener::from_std(listener)
                .unwrap();
            let mut incoming_connection_handler
                = incoming_connection_handler.clone();
            runtime.spawn(async move {
                loop {
                    if let Ok((sock, _)) = listener.accept().await {
                        incoming_connection_handler(Box::new(sock));
                    }
                }
            });
        }
        true
    }) { return None }
    Some(Invocation {
        runtime
    })
}
