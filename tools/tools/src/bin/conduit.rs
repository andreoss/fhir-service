use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::thread;

const USAGE: &str = "usage: conduit <target address> [listen address]";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(target) = args.first().cloned() else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    };
    let near = args.get(1).cloned().unwrap_or("127.0.0.1:0".to_owned());
    let listener = match TcpListener::bind(near) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("the conduit cannot listen: {error}");
            std::process::exit(1);
        }
    };
    match listener.local_addr() {
        Ok(address) => println!("carrying to {target} on {address}"),
        Err(error) => {
            eprintln!("the conduit has no address: {error}");
            std::process::exit(1);
        }
    }
    for incoming in listener.incoming() {
        let Ok(near) = incoming else { continue };
        let target = target.clone();
        thread::spawn(move || carry(near, &target));
    }
}

fn carry(near: TcpStream, target: &str) {
    let Ok(far) = TcpStream::connect(target) else {
        return;
    };
    let pairs = [
        (near.try_clone(), far.try_clone()),
        (far.try_clone(), near.try_clone()),
    ];
    let mut running = Vec::new();
    for (from, to) in pairs {
        let (Ok(mut from), Ok(mut to)) = (from, to) else {
            continue;
        };
        running.push(thread::spawn(move || {
            let mut held = [0u8; 16 * 1024];
            loop {
                match from.read(&mut held) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        use std::io::Write;
                        if to.write_all(&held[..read]).is_err() {
                            break;
                        }
                    }
                }
            }
            let _ = to.shutdown(std::net::Shutdown::Both);
        }));
    }
    for one in running {
        let _ = one.join();
    }
}
